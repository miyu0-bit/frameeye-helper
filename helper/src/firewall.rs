//! Windows only: let VRChat receive OSC (UDP 9000) from the headset through Windows Defender Firewall.
//! VRChat's own firewall rule is often only allowed on "Public" networks, so OSC arriving over a
//! "Private" home network is dropped. The rule added here is narrow: inbound UDP 9000, from the local
//! network only, and only for VRChat.exe when it can be found.

#[cfg(windows)]
use std::path::PathBuf;

/// Where VRChat.exe is, from Steam's library list.
#[cfg(windows)]
pub fn find_vrchat() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam") {
        if let Ok(path) = key.get_value::<String, _>("SteamPath") {
            roots.push(PathBuf::from(path.replace('/', "\\")));
        }
    }
    if let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\WOW6432Node\Valve\Steam") {
        if let Ok(path) = key.get_value::<String, _>("InstallPath") {
            roots.push(PathBuf::from(path));
        }
    }
    roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));

    let mut libraries = roots.clone();
    for root in &roots {
        let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) else {
            continue;
        };
        for line in text.lines() {
            // "path"		"D:\\SteamLibrary"
            if let Some(rest) = line.trim().strip_prefix("\"path\"") {
                libraries.push(PathBuf::from(rest.trim().trim_matches('"').replace("\\\\", "\\")));
            }
        }
    }
    libraries
        .into_iter()
        .map(|library| library.join("steamapps").join("common").join("VRChat").join("VRChat.exe"))
        .find(|exe| exe.is_file())
}

#[cfg(windows)]
fn script(vrchat: Option<&std::path::Path>) -> String {
    let program = vrchat.map(|path| path.display().to_string().replace('\'', "''")).unwrap_or_default();
    format!(
        r#"$ErrorActionPreference = 'Stop'
$Host.UI.RawUI.WindowTitle = 'FrameEye Helper - Windows Firewall'
Write-Host 'FrameEye Helper: letting VRChat receive eye tracking (OSC, UDP port 9000) from your Steam Frame.'
Write-Host ''
$name = 'VRChat OSC from Steam Frame (FrameEye Helper)'
$program = '{program}'
try {{
    Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    $rule = @{{ DisplayName = $name; Direction = 'Inbound'; Action = 'Allow'; Protocol = 'UDP'; LocalPort = 9000; RemoteAddress = 'LocalSubnet'; Profile = 'Any' }}
    if ($program) {{ $rule.Program = $program }}
    New-NetFirewallRule @rule | Out-Null
    Write-Host "Added the firewall rule '$name'." -ForegroundColor Green
    if ($program) {{ Write-Host "  It lets only VRChat ($program) receive UDP port 9000, and only from your local network." }}
    else {{ Write-Host '  It opens only UDP port 9000, and only to your local network (VRChat.exe was not found).' }}
    Write-Host ''
    Write-Host 'Checking for rules that block VRChat...'
    $blocks = @(Get-NetFirewallRule -Direction Inbound -Action Block -Enabled True -ErrorAction SilentlyContinue |
        Where-Object {{ (($_ | Get-NetFirewallApplicationFilter).Program -like '*\VRChat.exe') }})
    if ($blocks.Count -gt 0) {{
        Write-Host 'Windows also has rules that BLOCK VRChat. A block rule wins over an allow rule:' -ForegroundColor Yellow
        $blocks | ForEach-Object {{ Write-Host ('  - ' + $_.DisplayName + '  (profile: ' + $_.Profile + ')') }}
        Write-Host '(These usually come from pressing "Cancel" on the firewall question when VRChat first started.)'
        $answer = Read-Host 'Turn these block rules off? Type y and press Enter for yes'
        if ($answer -match '^[yY]') {{ $blocks | Disable-NetFirewallRule; Write-Host 'Turned them off.' -ForegroundColor Green }}
    }} else {{
        Write-Host 'None found. Good.' -ForegroundColor Green
    }}
}} catch {{
    Write-Host "Something went wrong: $_" -ForegroundColor Red
}}
Write-Host ''
Read-Host 'Done. Press Enter to close this window'
"#
    )
}

/// Opens an elevated PowerShell window (Windows asks for permission first) that adds the rule.
#[cfg(windows)]
pub fn allow_vrchat_osc() -> Result<String, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let vrchat = find_vrchat();
    let path = std::env::temp_dir().join("frameeye-helper-firewall.ps1");
    // Windows PowerShell 5.1 reads a file without a BOM as ANSI, which garbles non-ASCII paths.
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script(vrchat.as_deref()).as_bytes());
    std::fs::write(&path, bytes).map_err(|error| format!("Couldn't write {}: {error}", path.display()))?;

    let wide = |text: &str| -> Vec<u16> { std::ffi::OsStr::new(text).encode_wide().chain(Some(0)).collect() };
    let parameters = format!("-NoProfile -ExecutionPolicy Bypass -File \"{}\"", path.display());
    let (verb, file, parameters) = (wide("runas"), wide("powershell.exe"), wide(&parameters));
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a value above 32 on success
    if result as usize > 32 {
        Ok(match vrchat {
            Some(exe) => format!("A PowerShell window is adding the rule for {}. Follow it there.", exe.display()),
            None => "VRChat.exe wasn't found, so the rule opens UDP 9000 to the local network only. Follow the PowerShell window.".into(),
        })
    } else {
        Err("Windows didn't run it (the permission question was probably answered with No).".into())
    }
}

#[cfg(not(windows))]
pub fn allow_vrchat_osc() -> Result<String, String> {
    Err("The firewall fix is only needed (and only works) on Windows.".into())
}
