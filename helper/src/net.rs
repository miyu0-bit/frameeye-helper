//! This PC's own addresses, and the "Find my headset" scan of the local network for SSH servers.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

/// Every address of this PC (including loopback).
pub fn local_ips() -> Vec<IpAddr> {
    if_addrs::get_if_addrs()
        .map(|interfaces| interfaces.into_iter().map(|interface| interface.ip()).collect())
        .unwrap_or_default()
}

/// Whether frameeyeosc's destination is this PC: one of its addresses (`local`, from `local_ips`), or
/// the address the headset sees this PC's SSH connection come from.
pub fn is_this_pc(target: &str, client_ip: Option<IpAddr>, local: &[IpAddr]) -> Option<bool> {
    let ip = target
        .parse::<SocketAddr>()
        .map(|addr| addr.ip())
        .or_else(|_| target.parse::<IpAddr>())
        .ok()?;
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    Some(Some(ip) == client_ip || local.contains(&ip))
}

#[derive(Clone, Debug)]
pub struct ScanHit {
    pub ip: Ipv4Addr,
    /// The SSH server's greeting, e.g. "SSH-2.0-OpenSSH_9.9".
    pub banner: String,
}

/// The /24 networks this PC is on (private IPv4 only, no link-local), with this PC's own address.
fn networks() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    let mut networks = Vec::new();
    for interface in if_addrs::get_if_addrs().unwrap_or_default() {
        let IpAddr::V4(ip) = interface.ip() else { continue };
        if ip.is_loopback() || ip.is_link_local() || !ip.is_private() {
            continue;
        }
        let [a, b, c, _] = ip.octets();
        let base = Ipv4Addr::new(a, b, c, 0);
        if !networks.iter().any(|(known, _)| *known == base) {
            networks.push((base, ip));
        }
    }
    networks
}

/// Look for SSH servers on the local /24 networks. `progress` gets a 0..1 fraction now and then.
pub async fn scan(port: u16, progress: impl Fn(f32) + Send + Sync + 'static) -> (Vec<ScanHit>, usize) {
    let networks = networks();
    let targets: Vec<Ipv4Addr> = networks
        .iter()
        .flat_map(|(base, own)| {
            let [a, b, c, _] = base.octets();
            (1..=254u8).map(move |d| Ipv4Addr::new(a, b, c, d)).filter(move |ip| ip != own)
        })
        .collect();
    let total = targets.len();
    let limit = Arc::new(Semaphore::new(96));
    let progress = Arc::new(progress);
    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::with_capacity(total);
    for ip in targets {
        let limit = limit.clone();
        let progress = progress.clone();
        let done = done.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = limit.acquire().await.ok()?;
            let hit = probe(ip, port).await;
            let finished = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if finished % 16 == 0 || finished == total {
                progress(finished as f32 / total.max(1) as f32);
            }
            hit
        }));
    }
    let mut hits = Vec::new();
    for task in tasks {
        if let Ok(Some(hit)) = task.await {
            hits.push(hit);
        }
    }
    hits.sort_by_key(|hit| (!hit.banner.contains("OpenSSH"), hit.ip));
    (hits, networks.len())
}

async fn probe(ip: Ipv4Addr, port: u16) -> Option<ScanHit> {
    let addr = SocketAddr::new(IpAddr::V4(ip), port);
    let mut stream = tokio::time::timeout(Duration::from_millis(700), TcpStream::connect(addr))
        .await
        .ok()?
        .ok()?;
    let mut buffer = [0u8; 128];
    let read = tokio::time::timeout(Duration::from_millis(1500), stream.read(&mut buffer)).await;
    let banner = match read {
        Ok(Ok(n)) if n > 0 => String::from_utf8_lossy(&buffer[..n]).lines().next().unwrap_or("").trim().to_string(),
        _ => String::new(),
    };
    // Only SSH servers (they greet first with "SSH-"); anything else on this port is not a headset.
    banner.starts_with("SSH-").then_some(ScanHit { ip, banner })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local server that greets like `greeting` and then closes.
    fn server(greeting: &'static [u8]) -> u16 {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(greeting);
            }
        });
        port
    }

    #[tokio::test]
    async fn probe_finds_ssh_servers_only() {
        let ssh = server(b"SSH-2.0-OpenSSH_9.9\r\n");
        let hit = probe(Ipv4Addr::LOCALHOST, ssh).await.expect("an SSH server is found");
        assert_eq!(hit.banner, "SSH-2.0-OpenSSH_9.9");
        let web = server(b"HTTP/1.1 400 Bad Request\r\n");
        assert!(probe(Ipv4Addr::LOCALHOST, web).await.is_none());
        // Nothing listening: refused quickly, no hit
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(probe(Ipv4Addr::LOCALHOST, closed).await.is_none());
    }

    #[test]
    fn loopback_targets_are_this_pc() {
        let local = local_ips();
        assert_eq!(is_this_pc("127.0.0.1:9000", None, &local), Some(true));
        assert_eq!(is_this_pc("[::ffff:127.0.0.1]:9000", None, &local), Some(true));
        assert_eq!(is_this_pc("203.0.113.9:9000", None, &local), Some(false));
        assert_eq!(is_this_pc("203.0.113.9:9000", Some("203.0.113.9".parse().unwrap()), &local), Some(true));
        assert_eq!(is_this_pc("not an address", None, &local), None);
    }
}
