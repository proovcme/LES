"""Small real PDFs produced without an optional PDF authoring dependency."""
from io import BytesIO
from PIL import Image


def write_pdf(path, pages, *, drawing_segments=0):
    """Accept ASCII text, literal PDF drawing operators, or None for a scan."""
    objects = [b'', b'']
    def add(data):
        objects.append(data)
        return len(objects)
    def stream(data, meta=b''):
        return b'<< /Length ' + str(len(data)).encode() + b' ' + meta + b' >>\nstream\n' + data + b'\nendstream'
    font = add(b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>')
    children = []
    for text in pages:
        resources = f'/Font << /F1 {font} 0 R >>'
        if text is None:
            data = BytesIO()
            with Image.new('RGB', (50, 50), 'black') as image:
                image.save(data, format='JPEG')
            img = add(stream(data.getvalue(), b'/Type /XObject /Subtype /Image /Width 50 /Height 50 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode'))
            resources += f' /XObject << /Im {img} 0 R >>'
            content = b'q 300 0 0 300 0 0 cm /Im Do Q'
        elif isinstance(text, bytes):
            content = text
        else:
            escaped = text.replace('\\', '\\\\').replace('(', '\\(').replace(')', '\\)')
            content = f'BT /F1 12 Tf 20 280 Td ({escaped}) Tj ET'.encode('ascii')
        if drawing_segments:
            content += b'\n' + b'\n'.join(f'{i%300} 0 m {i%300} 300 l S'.encode() for i in range(drawing_segments))
        content_id = add(stream(content))
        children.append(add(f'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << {resources} >> /Contents {content_id} 0 R >>'.encode()))
    objects[0] = b'<< /Type /Catalog /Pages 2 0 R >>'
    objects[1] = f'<< /Type /Pages /Count {len(children)} /Kids [{" ".join(f"{n} 0 R" for n in children)}] >>'.encode()
    result = bytearray(b'%PDF-1.4\n')
    offsets = []
    for i, obj in enumerate(objects, 1):
        offsets.append(len(result))
        result.extend(f'{i} 0 obj\n'.encode() + obj + b'\nendobj\n')
    xref = len(result)
    result.extend(f'xref\n0 {len(objects)+1}\n0000000000 65535 f \n'.encode())
    for offset in offsets:
        result.extend(f'{offset:010d} 00000 n \n'.encode())
    result.extend(f'trailer\n<< /Size {len(objects)+1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode())
    path.write_bytes(result)
    return path


def write_table_pdf(path, *, next_divider=150, notes=("", "")):
    pages = []
    for divider, caption, rows, note in (
        (150, "Table 7", (("Code", "Quantity"), ("RG-1", "32")), notes[0]),
        (next_divider, r"Table 7 \(continued\)", (("RG-2", "7654321"), ("RG-3", "19")), notes[1]),
    ):
        operators = [f"BT /F1 12 Tf 20 280 Td ({caption}) Tj ET"]
        operators.extend(f"{x} 220 m {x} 260 l S" for x in (20, divider, 280))
        operators.extend(f"20 {y} m 280 {y} l S" for y in (220, 240, 260))
        for index, row in enumerate(rows):
            operators.extend(f"BT /F1 10 Tf {x} {247 - index * 20} Td ({value}) Tj ET"
                             for x, value in zip((25, divider + 5), row))
        if note:
            for index, line in enumerate(note.splitlines()):
                operators.append(f"BT /F1 10 Tf 20 {190 - index * 16} Td ({line}) Tj ET")
        pages.append("\n".join(operators).encode("ascii"))
    return write_pdf(path, pages)
