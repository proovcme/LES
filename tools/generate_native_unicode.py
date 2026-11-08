"""Development-only frozen Unicode tables. Never needed by the installed Rust app."""
import argparse
import json
from pathlib import Path
import unicodedata as ud

VERSION = '15.1.0'
OUTPUT = Path(__file__).resolve().parents[1] / 'config/unicode/15.1.0.json'


def generate():
    if ud.unidata_version != VERSION:
        raise RuntimeError(f'Generator requires Unicode {VERSION}; got {ud.unidata_version}')
    result = {'schema': 'les.unicode.v1', 'version': VERSION,
              'nfkd': {}, 'casefold': {}, 'combining': {}, 'compose': {}, 'decimal_zeroes': []}
    for number in range(0x110000):
        if 0xD800 <= number <= 0xDFFF:
            continue
        char = chr(number)
        key = f'{number:X}'
        if ud.decimal(char, None) == 0:
            assert all(ud.decimal(chr(number + digit), None) == digit for digit in range(10))
            result['decimal_zeroes'].append(number)
        decomposition = ud.normalize('NFKD', char)
        if decomposition != char and not 0xAC00 <= number <= 0xD7A3:
            result['nfkd'][key] = decomposition
        if char.casefold() != char:
            result['casefold'][key] = char.casefold()
        if ud.combining(char):
            result['combining'][key] = ud.combining(char)
        canonical = ud.decomposition(char).split()
        if len(canonical) == 2 and not canonical[0].startswith('<'):
            pair = ''.join(chr(int(v, 16)) for v in canonical)
            if ud.normalize('NFC', pair) == char:
                result['compose']['.'.join(f'{ord(v):X}' for v in pair)] = number
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    value = generate()
    if args.check:
        if json.loads(OUTPUT.read_text(encoding='utf-8')) != value:
            raise SystemExit('Native Unicode tables differ from the frozen oracle')
    else:
        OUTPUT.write_text(json.dumps(value, ensure_ascii=False, separators=(',', ':')) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
