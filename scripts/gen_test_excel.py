#!/usr/bin/env python3
"""Generate watchdog_test_cases.xlsx from the TESTING_GUIDE.md case table.

No third-party deps: an .xlsx is a zip of OOXML parts, written here with
the standard library so it works offline. The guide's table encodes literal
pipes as ``\\|`` and line breaks as ``<br>``; we reverse both.

Three columns are *derived* rather than copied from the guide:

* 所属模块  — forced to the product name ``基于Rust语言的看门狗驱动`` (the guide
              keeps the requirement-traceability ID; the Excel is the QA view).
* 优先级    — collapsed to 高 / 中 / 低 by an explicit rubric over the actual
              test unit (see ``PRIORITY``), not a mechanical P0/P1 remap.
* 执行结果  — read back from the latest ``logs/*/tests.log`` files, matching
              each case's test function(s) and reporting 通过 / 失败 plus the
              real reboot / safe-skip nuances the logs record.
"""
import re
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GUIDE = ROOT / "TESTING_GUIDE.md"
LOGS = ROOT / "logs"
OUT = ROOT / "watchdog_test_cases.xlsx"

MODULE = "基于Rust语言的看门狗驱动"

# 优先级 rubric, keyed by case ID.  Decided by what the test unit actually
# exercises:
#   高 — core watchdog protection & safety + end-to-end + real-hardware reset
#   中 — interface conformance, per-driver capability, robustness
#   低 — pure observability: log-regex / version / chipset / action / param
PRIORITY = {
    # 高
    "C-01": "高", "C-05": "高", "C-06": "高", "C-07": "高", "C-08": "高",
    "DRV-01": "高", "DRV-04": "高",
    "GC-01": "高", "GC-02": "高", "GC-03": "高", "GC-04": "高",
    "LAB-01": "高", "LAB-02": "高",
    # 中
    "C-02": "中", "C-03": "中", "C-04": "中", "C-09": "中", "C-10": "中",
    "C-EXT-01": "中", "C-EXT-02": "中", "C-EXT-03": "中", "C-EXT-04": "中", "C-EXT-05": "中",
    "DRV-02": "中", "DRV-03": "中", "DRV-05": "中", "DRV-06": "中",
    "DRV-07": "中", "DRV-08": "中", "DRV-09": "中",
    # 低
    "DRV-10": "低", "SBSA-S-04": "低", "SBSA-EXT-06": "低",
    "SP5100-S-04": "低", "SP5100-EXT-06": "低", "SOFTDOG-EXT-06": "低",
}

# (header in output, source column index in the 11-column guide table, or a
#  sentinel for derived columns)
COLUMNS = [
    ("所属模块", "MODULE"),
    ("用例标题", 1),
    ("前置条件", 2),
    ("步骤", 3),
    ("预期结果", 4),
    ("优先级", "PRIORITY"),
    ("用例类型", 7),
    ("执行结果", "EXEC"),
]
SPLIT = re.compile(r"(?<!\\)\|")  # split on pipes that are not escaped
# test fn referenced in 步骤, e.g. ".../sbsa_gwdt-* regex_lite_self_check::matches_init_log_pattern --…"
FN = re.compile(r"/tmp/watchdog-test/[a-z0-9_]+-\*\s+([a-z0-9_]+(?:::[a-z0-9_]+)*)")


# ---------------------------------------------------------------------------
# Latest-log result derivation
# ---------------------------------------------------------------------------
def run_dirs_newest_first():
    dirs = [p for p in LOGS.glob("2026-*") if p.is_dir()]
    # dir name: 2026-05-25-<host>-<kind>-<HHMM> → sort by (date, HHMM)
    return sorted(dirs, key=lambda p: (p.name[:10], p.name.rsplit("-", 1)[-1]), reverse=True)


def load_log_signals():
    """fn -> {'status': 通过|失败, 'reboot': bool, 'skip': bool} from the newest
    run that exercised that fn (first hit wins)."""
    test_line = re.compile(r"^test (\S+) \.\.\.(.*)$", re.MULTILINE)
    reboot_line = re.compile(r"EXPECTED-REBOOT:\s*\S+::([a-z0-9_]+)")
    sig = {}
    for d in run_dirs_newest_first():
        log = d / "tests.log"
        if not log.exists():
            continue
        text = log.read_text(encoding="utf-8", errors="replace")
        reboot_fns = set(reboot_line.findall(text))
        for fn, rest in test_line.findall(text):
            entry = sig.setdefault(fn, {
                "status": "失败" if "FAILED" in rest else "通过",
                "reboot": fn in reboot_fns,
                "skip": "# SKIP" in rest,
            })
            # already recorded by a newer run → keep newer
        # reboot-only fns (no surviving 'test … ' line because SSH dropped)
        for fn in reboot_fns:
            sig.setdefault(fn, {"status": "通过", "reboot": True, "skip": False})
    return sig


def exec_result(fns, sig):
    looked = [(fn, sig.get(fn)) for fn in fns]
    covered = [r for _, r in looked if r]
    if not covered:
        return "未在最新日志中覆盖"
    if any(r["status"] == "失败" for r in covered):
        return "失败"
    if all(r.get("reboot") for r in covered):
        return "通过（触发真实复位）"
    if all(r.get("skip") for r in covered):
        return "通过（autonomous 安全跳过）"
    if any(r is None for _, r in looked):
        return "通过（部分用例未在最新日志覆盖）"
    return "通过"


# ---------------------------------------------------------------------------
# Guide parsing
# ---------------------------------------------------------------------------
def parse_rows(sig):
    rows = []
    for line in GUIDE.read_text(encoding="utf-8").splitlines():
        if not line.startswith("| 银河"):
            continue
        parts = SPLIT.split(line)[1:-1]  # drop empties from leading/trailing |
        cells = [c.strip().replace("\\|", "|").replace("<br>", "\n") for c in parts]
        if len(cells) != 11:
            sys.exit(f"expected 11 cells, got {len(cells)} in: {line[:80]}…")
        case_id = cells[1].split()[0]
        fns = FN.findall(cells[3].replace("\n", " "))
        derived = {
            "MODULE": MODULE,
            "PRIORITY": PRIORITY.get(case_id, "中"),
            "EXEC": exec_result(fns, sig),
        }
        if case_id not in PRIORITY:
            print(f"WARN: no priority mapping for {case_id}, defaulting 中", file=sys.stderr)
        rows.append([derived[src] if isinstance(src, str) else cells[src]
                     for _, src in COLUMNS])
    return rows


# ---------------------------------------------------------------------------
# Minimal OOXML (.xlsx) writer
# ---------------------------------------------------------------------------
def esc(s):
    s = s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")
    return "".join(ch for ch in s if ch in "\n\t\r" or ord(ch) >= 0x20)


def col_ref(idx):  # 0 -> A, 25 -> Z, 26 -> AA
    ref = ""
    idx += 1
    while idx:
        idx, rem = divmod(idx - 1, 26)
        ref = chr(65 + rem) + ref
    return ref


def cell_xml(c, r, text, style):
    return (f'<c r="{col_ref(c)}{r}" s="{style}" t="inlineStr">'
            f'<is><t xml:space="preserve">{esc(text)}</t></is></c>')


def sheet_xml(headers, rows):
    out = ['<?xml version="1.0" encoding="UTF-8" standalone="yes"?>',
           '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">']
    out.append(f'<dimension ref="A1:{col_ref(len(headers) - 1)}{len(rows) + 1}"/>')
    out.append('<sheetViews><sheetView workbookViewId="0">'
               '<pane ySplit="1" topLeftCell="A2" activePane="bottomLeft" state="frozen"/>'
               '</sheetView></sheetViews>')
    out.append('<sheetFormatPr defaultRowHeight="15"/>')
    widths = [26, 26, 60, 70, 60, 8, 16, 24]
    out.append("<cols>")
    for i, w in enumerate(widths, start=1):
        out.append(f'<col min="{i}" max="{i}" width="{w}" customWidth="1"/>')
    out.append("</cols>")
    out.append("<sheetData>")
    out.append('<row r="1">' + "".join(cell_xml(c, 1, h, 1) for c, h in enumerate(headers)) + "</row>")
    for ri, row in enumerate(rows, start=2):
        out.append(f'<row r="{ri}">' + "".join(cell_xml(c, ri, v, 2) for c, v in enumerate(row)) + "</row>")
    out.append("</sheetData></worksheet>")
    return "".join(out)


STYLES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<fonts count="2"><font><sz val="11"/><name val="Calibri"/></font>
<font><b/><sz val="11"/><name val="Calibri"/></font></fonts>
<fills count="3"><fill><patternFill patternType="none"/></fill>
<fill><patternFill patternType="gray125"/></fill>
<fill><patternFill patternType="solid"><fgColor rgb="FFD9E1F2"/></patternFill></fill></fills>
<borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>
<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
<cellXfs count="3">
<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
<xf numFmtId="0" fontId="1" fillId="2" borderId="0" xfId="0" applyFont="1" applyFill="1"><alignment horizontal="center" vertical="center" wrapText="1"/></xf>
<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0" applyAlignment="1"><alignment vertical="top" wrapText="1"/></xf>
</cellXfs>
<cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles>
</styleSheet>"""

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"""

RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

WORKBOOK = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="测试用例" sheetId="1" r:id="rId1"/></sheets>
</workbook>"""

WB_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"""


def main():
    sig = load_log_signals()
    rows = parse_rows(sig)
    headers = [h for h, _ in COLUMNS]
    with zipfile.ZipFile(OUT, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", CONTENT_TYPES)
        z.writestr("_rels/.rels", RELS)
        z.writestr("xl/workbook.xml", WORKBOOK)
        z.writestr("xl/_rels/workbook.xml.rels", WB_RELS)
        z.writestr("xl/styles.xml", STYLES)
        z.writestr("xl/worksheets/sheet1.xml", sheet_xml(headers, rows))
    # console summary
    from collections import Counter
    pri = Counter(r[5] for r in rows)
    ex = Counter(r[7] for r in rows)
    print(f"wrote {OUT.relative_to(ROOT)} with {len(rows)} test cases, {len(headers)} columns")
    print("  所属模块:", MODULE)
    print("  优先级 :", dict(pri))
    print("  执行结果:", dict(ex))


if __name__ == "__main__":
    main()
