#!/usr/bin/env python3
# KYB — CLI tag presence regression checks
# Copyright (c) 2026 Alexander Panasenko
# Contact: alex@prod.codes
# Author: https://prod.codes/about/
# Project: https://github.com/alex09x/kyb
# SPDX-License-Identifier: MIT

import json
import os
from pathlib import Path
import subprocess
import tempfile

CLI = Path(__file__).resolve().parent.parent / 'skills/kyb/bin/kyb'

with tempfile.TemporaryDirectory(prefix='kyb-cli-tags-') as directory:
    mock = Path(directory) / 'curl'
    mock.write_text('''#!/usr/bin/env python3
import json, sys
args = sys.argv[1:]
body = args[args.index('-d') + 1] if '-d' in args else '{}'
# the CLI reads the body from -o and the HTTP status from -w
if '-o' in args:
    open(args[args.index('-o') + 1], 'w').write(body)
else:
    print(body)
if '-w' in args:
    sys.stdout.write('200')
''')
    mock.chmod(0o755)
    env = dict(os.environ, PATH=directory + os.pathsep + os.environ['PATH'],
               KYB_URL='http://127.0.0.1:9', KYB_TOKEN='')
    for command, key, required in [
        ('incident', 'inc-2026-10-20-tags', ['--service', 'hl-node', '--severity', 'low']),
        ('task', 'task-tags', []),
    ]:
        base = ['bash', str(CLI), command, '--key', key, '--title', 'Tagged entry'] + required
        for flags, expected in [([], None), (['--tags', 'hyperliquid,replay'], ['hyperliquid', 'replay']), (['--tags', ''], [])]:
            output = subprocess.run(base + flags, input='Original text', text=True,
                                    env=env, capture_output=True, check=True)
            payload = json.loads(output.stdout)
            if expected is None:
                assert 'tags' not in payload, (command, payload)
            else:
                assert payload['tags'] == expected, (command, payload)
            assert payload['body'] == 'Original text'
print('PASS: incident/task CLI distinguishes omitted tags, explicit tags and clearing')
