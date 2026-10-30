"""Exercise startup update failures and real Windows self-replacement in isolated folders.
Pass a DEBUG build: only it accepts the loopback API fixture. No game is ever launched.
"""
from pathlib import Path
import argparse
import hashlib
import http.server
import io
import json
import os
import shutil
import subprocess
import tempfile
import threading
import time
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument('--launcher', required=True)
args = parser.parse_args()
launcher = Path(args.launcher).resolve()
scratch = Path(tempfile.mkdtemp(prefix='horizon-startup-tests-'))
csc = Path(os.environ['WINDIR']) / 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'

def compile_fixture(name, version):
    source = scratch / (name + '.cs')
    binary = scratch / (name + '-fixture.exe')
    source.write_text('''using System; using System.IO;
class Fixture { static int Main(string[] args) {
if (args.Length == 1 && args[0] == "--version") { Console.WriteLine("NAME VERSION"); return 0; }
string marker = Environment.GetEnvironmentVariable("HORIZON_TEST_RESTART_MARKER");
if (!String.IsNullOrEmpty(marker)) File.WriteAllText(marker, "restarted");
return 0; }}'''.replace('NAME', name).replace('VERSION', version), encoding='utf-8')
    subprocess.run([str(csc), '/nologo', '/target:exe', '/out:' + str(binary), str(source)], check=True, capture_output=True)
    return binary

official = compile_fixture('horizon-launcher', '0.2.28')
replacement = compile_fixture('horizon-offline', '0.9.0')
state = {'mode': 'current', 'downloads': []}

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        mode = state['mode']
        if mode == 'offline':
            self.send_error(503); return
        if self.path.startswith('/repos/'):
            if mode == 'malformed':
                data = b'not json'
            else:
                ours = '/holodse-web/' in self.path
                version = '0.8.0' if mode in {'current', 'bad-signature'} else '0.9.0'
                tag = 'v' + version if ours else 'launcher@' + ('0.2.29' if mode == 'bad-signature' else '0.2.28')
                name = 'Horizon-Offline-' + version + '.zip' if ours else 'horizon-launcher-windows-x64.zip'
                archive = state['archive']
                assets = [{'name': name, 'size': len(archive) + (1 if mode == 'truncated-download' else 0), 'digest': 'sha256:' + ('0' * 64 if mode == 'bad-hash' else hashlib.sha256(archive).hexdigest()), 'browser_download_url': state['base'] + '/download.zip'}]
                if mode == 'no-digest': assets[0]['digest'] = None
                if mode == 'foreign-url': assets[0]['browser_download_url'] = 'https://example.invalid/download.zip'
                if not ours and mode == 'bad-signature':
                    assets += [{'name': x, 'size': 10, 'digest': None, 'browser_download_url': state['base'] + '/' + x} for x in ['SHA256SUMS', 'SHA256SUMS.sig']]
                release = {'tag_name': tag, 'draft': False, 'prerelease': mode == 'prerelease' and ours, 'assets': assets}
                data = json.dumps(release if ours else [release]).encode()
        elif self.path == '/download.zip':
            state['downloads'].append(self.path)
            data = state['archive']
        elif self.path == '/SHA256SUMS':
            data = b'a' * 64 + b'  horizon-launcher-windows-x64.zip\n'
        elif self.path == '/SHA256SUMS.sig':
            data = b'unsigned\n'
        else:
            self.send_error(404); return
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
state['base'] = 'http://127.0.0.1:' + str(server.server_port)
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
checks = []
try:
    for mode in ['current', 'offline', 'malformed', 'prerelease', 'bad-hash', 'no-digest', 'foreign-url', 'truncated-download', 'wrong-version', 'missing-exe', 'duplicate-exe', 'bad-signature', 'self-update']:
        state['mode'] = mode
        state['downloads'] = []
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, 'w', zipfile.ZIP_DEFLATED) as z:
            z.writestr('other.exe' if mode == 'missing-exe' else 'horizon-offline.exe', (official if mode == 'wrong-version' else replacement).read_bytes())
            if mode == 'duplicate-exe':
                z.writestr('nested/horizon-offline.exe', replacement.read_bytes())
        state['archive'] = buffer.getvalue()
        root = scratch / mode
        root.mkdir()
        exe = root / 'horizon-offline.exe'
        shutil.copy2(launcher, exe)
        shutil.copy2(official, root / 'horizon-launcher.exe')
        settings = root / 'horizon-rust.json'
        settings.write_text('{"name":"FixturePlayer","minecraft_dir":null,"launch":null}', encoding='utf-8')
        game = root / 'Horizon Offline Data/game'
        game.mkdir(parents=True)
        (game / 'options.txt').write_text('lang:en_us\n', encoding='utf-8')
        (game / 'player-data.txt').write_text('preserve me', encoding='utf-8')
        before = {p: p.read_bytes() for p in [settings, game/'options.txt', game/'player-data.txt', root/'horizon-launcher.exe']}
        env = {**os.environ, 'HORIZON_OFFLINE_TEST_API': state['base'], 'NO_COLOR': '1',
               'HORIZON_RUST_STANDALONE': '1', 'HORIZON_TEST_RESTART_MARKER': str(root/'restarted.txt')}
        env.pop('HORIZON_OFFLINE_NO_UPDATE', None)
        for key in ['HORIZON_HOME', 'HORIZON_LAUNCHER_HOME', 'XDG_DATA_HOME', 'XDG_CONFIG_HOME']:
            env[key] = str(root / key)
        result = subprocess.run([str(exe), '--menu'], input='0\n', capture_output=True, text=True, encoding='utf-8', errors='replace', env=env, timeout=35)
        output = result.stdout + result.stderr
        (root / 'output.txt').write_text(output, encoding='utf-8')
        assert result.returncode == 0, (mode, output)
        assert '\x1b' not in output, (mode, 'ANSI leaked into redirected output')
        for path, data in before.items():
            assert path.read_bytes() == data, (mode, 'player/original data changed', path)
        if mode == 'self-update':
            deadline = time.monotonic() + 5
            while not (root/'restarted.txt').exists() and time.monotonic() < deadline:
                time.sleep(.05)
            assert (root/'restarted.txt').exists(), output
            assert exe.read_bytes() == replacement.read_bytes()
            assert (root/'.horizon-updates/horizon-offline.exe.previous').read_bytes() == launcher.read_bytes()
        else:
            assert exe.read_bytes() == launcher.read_bytes(), (mode, 'invalid update replaced EXE')
            assert not (root/'restarted.txt').exists()
        if mode in {'current', 'prerelease'}:
            assert not state['downloads'], (mode, 'unnecessary download')
        checks.append(mode)
        print('PASS:', mode)
finally:
    server.shutdown()
    server.server_close()
print(json.dumps({'scenarios': checks, 'evidence': str(scratch), 'game_started': False}))
