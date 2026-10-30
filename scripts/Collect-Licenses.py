"""Generate the notices embedded by --licenses, using the locked local Cargo sources."""
from pathlib import Path
import os
import tomllib
import subprocess

root = Path(__file__).resolve().parent.parent
cargo = Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))
registries = list((cargo / 'registry/src').glob('*'))
tree = subprocess.check_output(['cargo', 'tree', '--locked', '--manifest-path', str(root / 'Cargo.toml'), '--target', 'x86_64-pc-windows-gnu', '--prefix', 'none', '--format', '{p}', '--edges', 'normal,build'], text=True)
active = {tuple(line.split()[:2]) for line in tree.splitlines()}
parts = ['HORIZON OFFLINE — THIRD-PARTY LICENSES\nView with: horizon-offline.exe --licenses\n']
for path in [root / 'LICENSE', root / 'THIRD_PARTY_NOTICES.md', *sorted((root / 'licenses').glob('*'))]:
    parts.append(f'\n{"=" * 72}\n{path.relative_to(root)}\n{"=" * 72}\n' + path.read_text(encoding='utf-8-sig'))
missing = []
for package in tomllib.loads((root / 'Cargo.lock').read_text(encoding='utf-8'))['package']:
    if 'source' not in package:
        continue
    if (package['name'], 'v' + package['version']) not in active:
        continue
    name = f"{package['name']}-{package['version']}"
    source = next((p / name for p in registries if (p / name).is_dir()), None)
    if not source:
        missing.append(name + ': crate source missing')
        continue
    licenses = [p for p in source.iterdir() if p.is_file() and p.name.upper().startswith(('LICENSE', 'LICENCE', 'COPYING', 'COPYRIGHT', 'NOTICE', 'UNLICENSE'))]
    # Some crates keep the complete text in a dedicated licenses directory.
    for directory in ['licenses', 'LICENSES']:
        if (source / directory).is_dir():
            licenses += [p for p in (source / directory).rglob('*') if p.is_file()]
    if not licenses:
        missing.append(name + ': license text missing')
    for path in sorted(licenses):
        parts.append(f'\n{"=" * 72}\n{name} / {path.relative_to(source)}\n{"=" * 72}\n' + path.read_text(encoding='utf-8-sig', errors='strict'))
if missing:
    raise SystemExit('\n'.join(missing))
output = root / 'assets/THIRD_PARTY_LICENSES.txt'
output.write_text('\n'.join(parts) + '\n', encoding='utf-8')
print(f'Embedded notices: {output.stat().st_size} bytes')
