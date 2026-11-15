import fcntl
import json
import os
import pathlib
import pty
import select
import signal
import struct
import subprocess
import sys
import termios
import time

binary, repository, state = sys.argv[1:]
os.environ['PWD'] = repository
os.environ['TERM'] = 'xterm-256color'
bin_directory = pathlib.Path(state) / 'bin'
bin_directory.mkdir()
editor_log = pathlib.Path(state) / 'editor.jsonl'
editor = bin_directory / 'hx'
editor.write_text('#!' + sys.executable + '\n' +
    'import json, os, pathlib, sys\n' +
    'path = pathlib.Path(sys.argv[-1])\n' +
    'with open(' + repr(str(editor_log)) + ', "a") as log:\n' +
    '    log.write(json.dumps({"path": str(path), "args": sys.argv[1:], "content": path.read_bytes().decode(), "readonly": path.stat().st_mode & 0o222 == 0}) + "\\n")\n')
editor.chmod(0o755)
os.environ['PATH'] = str(bin_directory) + os.pathsep + os.environ['PATH']

arguments = ['difftool', '$left', '$right', '--agent', 'none', '--workspace-root', state]
command = ['jj', '--no-pager', '--config', 'merge-tools.prpr.program=' + json.dumps(binary),
           '--config', 'merge-tools.prpr.diff-args=' + json.dumps(arguments), 'diff', '--tool', 'prpr']
pid, terminal = pty.fork()
if pid == 0:
    os.chdir(repository)
    os.execvp(command[0], command)
fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 120, 0, 0))
output = bytearray()


def pump():
    if select.select([terminal], [], [], 0.05)[0]:
        try:
            output.extend(os.read(terminal, 65536))
        except OSError:
            pass


def wait_until(predicate):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        pump()
        if predicate():
            return
    raise AssertionError('Viewer timed out: ' + output.decode(errors='replace')[-3000:])


def client(*args, expect_success=True):
    process = subprocess.Popen([binary, '--workspace-root', state, 'session', *args],
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    deadline = time.monotonic() + 6
    while True:
        try:
            stdout, stderr = process.communicate(timeout=0.05)
            break
        except subprocess.TimeoutExpired:
            pump()
            if time.monotonic() > deadline:
                process.kill()
                process.communicate()
                raise AssertionError('Session client timed out')
    if expect_success:
        assert process.returncode == 0, stderr
        return json.loads(stdout)
    assert process.returncode != 0



try:
    wait_until(lambda: b'Local review.' in output)
    review = client('review', '--repo', repository, '--file', 'a.rs', '--include-patch')
    assert review['source']['kind'] == 'local'
    assert len(review['files']) == 1
    assert review['files'][0]['patch'].startswith('@@')
    client('navigate', '--repo', repository, '--file', 'a.rs', '--new-line', '1')
    os.write(terminal, b'c')
    wait_until(lambda: b'Ctrl-s save' in output)
    os.write(terminal, b'Why??\x08\x13')
    wait_until(lambda: len(client('comment', 'list', '--repo', repository)['notes']) == 1)
    notes = client('comment', 'list', '--repo', repository)['notes']
    assert notes[0]['anchor']['body'] == 'Why?'
    client('comment', 'add', '--repo', repository, '--reply-to', notes[0]['id'],
           '--revision', 'stale', '--body', 'Old response', expect_success=False)
    client('comment', 'add', '--repo', repository, '--reply-to', notes[0]['id'],
           '--revision', review['revision'], '--body', 'The diff changes the returned value.', '--author', 'codex')
    pump()
    assert len(client('comment', 'list', '--repo', repository)['notes']) == 2
    client('navigate', '--repo', repository, '--file', 'a.rs', '--new-line', '1')
    os.write(terminal, b'o')
    wait_until(lambda: editor_log.exists())
    client('navigate', '--repo', repository, '--file', 'a.rs', '--old-line', '1')
    os.write(terminal, b'o')
    wait_until(lambda: len(editor_log.read_text().splitlines()) == 2)
    locations = [json.loads(line) for line in editor_log.read_text().splitlines()]
    assert pathlib.Path(locations[0]['path']).resolve() == (pathlib.Path(repository) / 'a.rs').resolve()
    assert locations[0]['args'][0] == '+1'
    assert locations[0]['content'] == 'after\r\nshared\r\n'
    assert locations[1]['content'] == 'before\r\nshared\r\n'
    assert locations[1]['readonly']
    os.write(terminal, b'q')
    exited = []
    def finished():
        result = os.waitpid(pid, os.WNOHANG)
        if result[0]:
            exited.append(result[1])
            return True
        return False
    wait_until(finished)
    assert os.waitstatus_to_exitcode(exited[0]) == 0
    assert client('list') == []
finally:
    try:
        os.killpg(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    os.close(terminal)
