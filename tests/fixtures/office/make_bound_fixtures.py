#!/usr/bin/env python3
"""Generates the T62 bound fixtures: workbooks that are small, well formed and
hostile to a reader that lays a sheet out densely. All data is fictional.

Run from this directory:  python3 make_bound_fixtures.py
"""
import os
import zipfile

# Pinned so a regeneration reproduces the committed files byte for byte;
# zipfile would otherwise stamp each entry with the current time.
STAMP = (2026, 9, 12, 0, 0, 0)


def put(z, name, data):
    info = zipfile.ZipInfo(name, date_time=STAMP)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = 0o644 << 16
    z.writestr(info, data)

CT = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>'''

RELS = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>'''

WB = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Regions" sheetId="1" r:id="rId1"/></sheets>
</workbook>'''

WBRELS = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>'''


def inline(ref, text):
    return '<c r="%s" t="inlineStr"><is><t>%s</t></is></c>' % (ref, text)


def number(ref, v):
    return '<c r="%s"><v>%s</v></c>' % (ref, v)


def sheet(dim, rows):
    return ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
            '<dimension ref="%s"/><sheetData>%s</sheetData></worksheet>' % (dim, "".join(rows)))


def write_xlsx(name, sheet_xml):
    with zipfile.ZipFile(name, "w", zipfile.ZIP_DEFLATED) as z:
        put(z, "[Content_Types].xml", CT)
        put(z, "_rels/.rels", RELS)
        put(z, "xl/workbook.xml", WB)
        put(z, "xl/_rels/workbook.xml.rels", WBRELS)
        put(z, "xl/worksheets/sheet1.xml", sheet_xml)
    print("%-24s %8d bytes" % (name, os.path.getsize(name)))


HEAD = '<row r="1">' + inline("A1", "Region") + inline("B1", "Units") + '</row>'
BODY = ('<row r="2">' + inline("A2", "North") + number("B2", 1200) + '</row>'
        '<row r="3">' + inline("A3", "South") + number("B3", 940) + '</row>')

# far-corner: honest dimension, three real rows, and ONE real cell in the far
# corner of the sheet. A dense layout between the extremes asks for
# 1,048,576 x 16,384 x 32 bytes and the process aborts.
write_xlsx("far-corner.xlsx", sheet("A1:B3", [
    HEAD, BODY,
    '<row r="1048576">' + inline("XFD1048576", "stray") + '</row>',
]))

# declared-huge: the opposite shape. The declared used range is the whole
# sheet and only three rows exist, so a reader that trusts the declaration
# refuses a file that costs 300 microseconds to read.
write_xlsx("declared-huge.xlsx", sheet("A1:XFD1048576", [HEAD, BODY]))

# oversized-part: a worksheet part declaring more than the 64 MiB
# decompression cap, from a file of a few KB.
big = " " * (64 * 1024 * 1024 + 1)
write_xlsx("oversized-part.xlsx", sheet("A1:B3", [HEAD, BODY]) + big)

# ---------------------------------------------------------------- ODS

ODS_MANIFEST = '''<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
<manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/>
<manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>'''

ODS_HEAD = ('<?xml version="1.0" encoding="UTF-8"?>\n'
            '<office:document-content '
            'xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
            'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" '
            'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">'
            '<office:body><office:spreadsheet>')
ODS_TAIL = '</office:spreadsheet></office:body></office:document-content>'


def ods_cell(text=None, repeats=1):
    rep = ' table:number-columns-repeated="%d"' % repeats if repeats > 1 else ''
    if text is None:
        return '<table:table-cell%s/>' % rep
    return ('<table:table-cell%s office:value-type="string">'
            '<text:p>%s</text:p></table:table-cell>' % (rep, text))


def ods_row(cells, repeats=1):
    rep = ' table:number-rows-repeated="%d"' % repeats if repeats > 1 else ''
    return '<table:table-row%s>%s</table:table-row>' % (rep, "".join(cells))


def write_ods(name, rows):
    body = ODS_HEAD + '<table:table table:name="Regions">' + "".join(rows) + '</table:table>' + ODS_TAIL
    with zipfile.ZipFile(name, "w", zipfile.ZIP_DEFLATED) as z:
        put(z, "mimetype", "application/vnd.oasis.opendocument.spreadsheet")
        put(z, "META-INF/manifest.xml", ODS_MANIFEST)
        put(z, "content.xml", body)
    print("%-24s %8d bytes" % (name, os.path.getsize(name)))


SHEET_3X2 = [
    ods_row([ods_cell("Region"), ods_cell("Units")]),
    ods_row([ods_cell("North"), ods_cell("1200")]),
    ods_row([ods_cell("South"), ods_cell("940")]),
]

# The shape every LibreOffice save has: a trailing empty block declaring the
# rest of the sheet. It must READ, and read the same as the sheet without it.
write_ods("trailing-block.ods", SHEET_3X2 + [ods_row([ods_cell(repeats=16384)], repeats=1048573)])
write_ods("plain-3x2.ods", SHEET_3X2)

# One real cell 95 columns out on the last row: the content span is
# 1,048,576 x 95, which calamine lays out as 3.19 GB and aborts on.
write_ods("ods-far-corner.ods", [
    ods_row([ods_cell("Region")]),
    ods_row([ods_cell(repeats=1)], repeats=1048574),
    ods_row([ods_cell(repeats=94), ods_cell("stray")]),
])
