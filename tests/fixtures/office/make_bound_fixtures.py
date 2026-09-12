#!/usr/bin/env python3
"""Generates the T62 bound fixtures: workbooks that are small, well formed and
hostile to a reader that lays a sheet out densely. All data is fictional.

Run from this directory:  python3 make_bound_fixtures.py
"""
import os
import zipfile

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
        z.writestr("[Content_Types].xml", CT)
        z.writestr("_rels/.rels", RELS)
        z.writestr("xl/workbook.xml", WB)
        z.writestr("xl/_rels/workbook.xml.rels", WBRELS)
        z.writestr("xl/worksheets/sheet1.xml", sheet_xml)
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
