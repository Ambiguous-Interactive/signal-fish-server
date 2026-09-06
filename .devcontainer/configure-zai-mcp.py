"""Update only repository-owned Z.AI Codex blocks; never persist credentials."""
import json
from pathlib import Path
import re
import sys
import tomllib


def configure(path):
    original = path.read_text()
    text = re.sub(r'^# >>> signal-fish zai .* mcp >>>\n.*?^# <<< signal-fish zai .* mcp <<<\n?',
                  '', original, flags=re.M | re.S)
    tables = tomllib.loads(text).get('mcp_servers', {})
    launcher = str(Path(__file__).resolve().with_name('zai-mcp.mjs'))
    for name in ('vision', 'web-search', 'web-reader', 'zread'):
        table = 'zai_' + name.replace('-', '_')
        if table in tables:
            continue
        label = name.replace('-', ' ')
        text = text.rstrip() + f'\n\n# >>> signal-fish zai {label} mcp >>>\n'
        text += f'[mcp_servers.{table}]\ncommand = "node"\n'
        text += 'args = ' + json.dumps([launcher, name]) + '\n'
        text += 'env_vars = ["Z_AI_API_KEY"]\n'
        text += f'# <<< signal-fish zai {label} mcp <<<\n'
    if text != original:
        path.write_text(text)


if __name__ == '__main__':
    configure(Path(sys.argv[1]))
