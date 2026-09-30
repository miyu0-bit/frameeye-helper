#!/usr/bin/env python3
"""Writes THIRD_PARTY_LICENSES.md: the licenses of the Rust crates built into FrameEyeHelper (Windows and
Linux) and the preview's fake-eye-server. Crates with the same license text are listed together.

    CARGO_HOME=build/cargo-home python3 build/third-party-licenses.py > THIRD_PARTY_LICENSES.md
"""
import hashlib
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROJECTS = [
    ("helper", ["x86_64-pc-windows-gnu", "x86_64-unknown-linux-gnu"]),
    ("preview/fake-eye-server", ["x86_64-unknown-linux-gnu"]),
]
LICENSE_NAMES = ("license", "licence", "copying", "notice", "unlicense")


def linked_packages(project, target):
    """Packages reachable from the project through normal dependencies (build scripts' own deps are left out)."""
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", target],
        cwd=os.path.join(ROOT, project), check=True, capture_output=True, text=True,
    ).stdout
    meta = json.loads(out)
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = meta["resolve"]["root"]
    seen, stack = set(), [root]
    while stack:
        node = nodes[stack.pop()]
        for dep in node["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]) and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                stack.append(dep["pkg"])
    return [packages[i] for i in seen]


def license_files(package):
    folder = os.path.dirname(package["manifest_path"])
    candidates = [(name, os.path.join(folder, name)) for name in sorted(os.listdir(folder))]
    # egui's font crate keeps the licenses of the fonts it embeds next to them
    fonts = os.path.join(folder, "fonts")
    if os.path.isdir(fonts):
        candidates += [(f"fonts/{name}", os.path.join(fonts, name)) for name in sorted(os.listdir(fonts)) if name.endswith(".txt")]
    found = []
    for name, path in candidates:
        base = os.path.basename(name).lower()
        if os.path.isfile(path) and (base.startswith(LICENSE_NAMES) or name.startswith("fonts/")):
            with open(path, encoding="utf-8", errors="replace") as f:
                found.append((name, f.read().strip()))
    return found


def main():
    crates = {}
    for project, targets in PROJECTS:
        for target in targets:
            for package in linked_packages(project, target):
                if package["source"] is None:  # our own crates
                    continue
                crates[(package["name"], package["version"])] = package
    groups = {}  # text hash -> (text, [crate labels])
    no_file = []
    for (name, version), package in sorted(crates.items()):
        label = f"{name} {version}"
        files = license_files(package)
        if not files:
            authors = ", ".join(package.get("authors") or []) or "its authors"
            source = package.get("repository") or package.get("homepage") or ""
            no_file.append(f"{label}: {package.get('license') or 'no license field'}, (c) {authors} {source}".rstrip())
            continue
        for filename, text in files:
            key = hashlib.sha256(text.encode()).hexdigest()
            groups.setdefault(key, (filename, text, []))[2].append(label)

    print("# Third-party licenses\n")
    print("FrameEye Helper is MIT licensed (see LICENSE). Its binaries contain the Rust crates below; each")
    print("crate's license text follows, crates with the same text grouped together.\n")
    print("frameeyeosc itself (bundled in the helper, and built into the Linux preview) is MIT, (c) konsti219 and")
    print("sasaken1102r; its own third-party licenses ship inside its release and in its repository.\n")
    print(f"{len(crates)} crates.\n")
    for filename, text, labels in sorted(groups.values(), key=lambda g: g[2][0]):
        print("---\n")
        print(f"**{', '.join(labels)}** ({filename})\n")
        print("```")
        print(text.replace("```", "'''"))
        print("```\n")
    if no_file:
        print("---\n")
        print("These crates publish no license file; their license comes from their manifest. The standard")
        print("texts (MIT, Apache-2.0, ...) appear above with other crates, and at https://spdx.org/licenses/.\n")
        for label in no_file:
            print(f"- {label}")


if __name__ == "__main__":
    sys.exit(main())
