"""Observe real cgroup telemetry with disposable files and a small child process."""
from __future__ import annotations

import argparse
import base64
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument('--expected-cpus', type=float, required=True)
    parser.add_argument('--expected-memory-mib', type=int, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='subforge-resources-') as temporary:
        root = Path(temporary)
        media, data = root / 'media', root / 'data'
        media.mkdir(); data.mkdir()
        source = media / 'fixture.mkv'
        track = root / 'track.srt'
        track.write_text(
            '1\n00:00:00,000 --> 00:00:01,000\nDisposable subtitle fixture\n',
            encoding='utf-8',
        )
        subprocess.run(
            ['/usr/bin/mkvmerge', '-q', '-o', str(source), '--language', '0:eng', str(track)],
            check=True,
            capture_output=True,
            timeout=5,
        )
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        environment = os.environ.copy()
        environment.update(CONVERTER_UI_PASSWORD='fixture-resource-password-long', CONVERTER_ALLOWED_HOSTS='127.0.0.1,localhost', CONVERTER_ENABLE_OCR='false', CONVERTER_FFPROBE='/usr/bin/ffprobe', CONVERTER_MKVMERGE='/usr/bin/mkvmerge')
        server = subprocess.Popen(['/usr/local/bin/subforge', '--host', '127.0.0.1', '--port', str(port), '--root', str(media), '--data', str(data), '--no-auto-scan'], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        child = None
        authorization = 'Basic ' + base64.b64encode(b'admin:fixture-resource-password-long').decode()
        csrf = ''

        def api(path: str, body: dict | None = None) -> dict:
            headers = {'Authorization': authorization}
            payload = None
            if body is not None:
                headers.update({'Content-Type': 'application/json', 'X-CSRF-Token': csrf})
                payload = json.dumps(body).encode()
            request = urllib.request.Request(f'http://127.0.0.1:{port}' + path, payload, headers)
            with urllib.request.urlopen(request, timeout=3) as response:
                return json.loads(response.read(2 * 1024 * 1024))

        try:
            deadline = time.monotonic() + 10
            while True:
                try:
                    csrf = api('/api/session')['csrf_token']
                    break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise RuntimeError('Disposable resource server did not become ready')
                    time.sleep(0.05)
            first = api('/api/system')
            usage = first['resource_usage']
            assert usage['cpu']['source'] in ('cgroup_v2', 'cgroup_v1')
            assert usage['cpu']['quota_cores'] == args.expected_cpus
            assert usage['memory']['cgroup_limit_bytes'] == args.expected_memory_mib * 1048576
            assert first['resource_policy']['conversion_workers'] == 1
            assert first['resource_policy']['ocr_threads'] == 1
            assert usage['cpu']['usage_sample_state'] == 'collecting'
            assert usage['cpu']['usage_percent'] is None
            baseline = usage['memory']['cgroup_current_bytes']
            parent_rss = usage['memory']['process_rss_bytes']
            child = subprocess.Popen([sys.executable, '-c', 'import time; allocation=bytearray(32*1024*1024); print("ready",flush=True); time.sleep(10)'], stdout=subprocess.PIPE, text=True)
            assert child.stdout.readline().strip() == 'ready'
            time.sleep(3.2)
            second = api('/api/system')
            measured = second['resource_usage']
            assert second['uptime_seconds'] > first['uptime_seconds']
            assert measured['cpu']['usage_sample_state'] == 'available'
            assert measured['cpu']['usage_percent'] >= 0
            assert measured['cpu']['effective_cores'] <= args.expected_cpus
            assert measured['memory']['cgroup_current_bytes'] >= baseline + 16 * 1048576
            assert abs(measured['memory']['process_rss_bytes'] - parent_rss) < 8 * 1048576
            job = api('/api/jobs', {'kind': 'inspect', 'paths': [str(source)]})['job']['id']
            deadline = time.monotonic() + 5
            finished = None
            while finished is None:
                finished = next(
                    (item for item in api('/api/status')['history'] if item['id'] == job),
                    None,
                )
                if finished is not None:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError('Disposable metadata inspection did not finish')
                time.sleep(0.05)
            assert finished['status'] == 'completed', {
                'status': finished.get('status'),
                'error': finished.get('error'),
                'summary': finished.get('summary'),
                'error_groups': finished.get('file_errors'),
                'errors': finished.get('errors'),
                'current': finished.get('current'),
            }
            inspected = api('/api/inventory')
            source_errors = [
                {'error': item.get('error')}
                for item in inspected.get('items', [])
                if item.get('path') == str(source) and item.get('error')
            ]
            assert finished.get('summary', {}).get('inspected') == 1, {
                'summary': finished.get('summary'),
                'error': finished.get('error'),
                'error_groups': finished.get('file_errors'),
                'errors': finished.get('errors'),
                'current': finished.get('current'),
                'inventory_errors': source_errors,
            }
            cached = api('/api/system')['resource_usage']['metadata_cache']
            assert cached['entries'] == 1 and cached['accounted_bytes'] > 0, {
                'cache': cached,
                'job_status': finished.get('status'),
                'job_summary': finished.get('summary'),
                'job_error': finished.get('error'),
            }
            assert cached['max_entries'] == first['resource_policy']['metadata_cache_max_entries']
            print(json.dumps({'cpu_quota_cores': usage['cpu']['quota_cores'], 'memory_limit_mib': args.expected_memory_mib, 'uptime_advances': True, 'cpu_sample_advances': True, 'container_counts_child_memory': True, 'process_rss_excludes_child_memory': True, 'metadata_usage_advances': True}))
        finally:
            if child is not None:
                child.terminate(); child.wait(timeout=3)
            server.terminate(); server.wait(timeout=5)


if __name__ == '__main__':
    main()
