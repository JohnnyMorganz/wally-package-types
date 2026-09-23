#!/usr/bin/env python3
# Run in root of repository
# scripts/release.py <version number, without leading v>

import subprocess
import sys

CARGO_TOML_FILE = "Cargo.toml"

assert len(sys.argv) == 2, "Usage: scripts/release.py <version number>"
VERSION = sys.argv[1]

# Update version in Cargo.toml
new_cargo_toml_lines: list[str] = []
with open(CARGO_TOML_FILE, "r") as file:
    for line in file:
        if line.startswith("version = "):
            new_cargo_toml_lines.append(f'version = "{VERSION}"\n')
        else:
            new_cargo_toml_lines.append(line)

with open(CARGO_TOML_FILE, "w") as file:
    file.writelines(new_cargo_toml_lines)

# Update Cargo.lock to match
subprocess.run(
    ["cargo", "update", "-p", "wally-package-types", "--precise", VERSION],
    check=True,
)

# Commit
subprocess.run(["git", "add", "Cargo.toml", "Cargo.lock"], check=True)
subprocess.run(["git", "commit", "-m", f"v{VERSION}"], check=True)

# Tag
subprocess.run(["git", "tag", "-a", f"v{VERSION}", "-m", f"v{VERSION}"], check=True)

# Push
subprocess.run(["git", "push"], check=True)
subprocess.run(["git", "push", "--tags"], check=True)
