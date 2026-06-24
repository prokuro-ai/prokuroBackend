import os, re, time, hashlib, requests, pandas as pd
from pathlib import Path

OUT = Path("bom_data")
RAW = OUT / "raw"
OUT.mkdir(exist_ok=True)
RAW.mkdir(exist_ok=True)

TOKEN = os.getenv("GITHUB_TOKEN")
if not TOKEN:
    raise SystemExit("Missing GITHUB_TOKEN. Run: export GITHUB_TOKEN=YOUR_TOKEN")

HEADERS = {
    "Authorization": f"Bearer {TOKEN}",
    "Accept": "application/vnd.github+json",
    "User-Agent": "bom-harvester"
}

QUERIES = [
    'filename:bill-of-materials',
    'filename:bill_of_materials',
    'filename:billofmaterials',
    'filename:BillOfMaterials',
    'filename:bill-materials',
    'filename:bill_materials',
    'filename:materials-bill',
    'filename:materials_bill',
    '"bill of materials" path:*.csv',
    '"bill-of-materials" path:*.csv',
    '"bill_of_materials" path:*.csv',
]

EXTS = {".csv", ".tsv", ".txt", ".xlsx", ".xls"}

def says_bill_of_materials(path):
    stem = Path(path).stem.lower()
    compact = re.sub(r"[^a-z0-9]", "", stem)
    spaced = re.sub(r"[^a-z0-9]+", " ", stem).strip()

    return (
        "billofmaterials" in compact
        or re.search(r"\bbill\s+of\s+materials\b", spaced)
        or re.search(r"\bbill\s+materials\b", spaced)
        or re.search(r"\bmaterials\s+bill\b", spaced)
    )

def safe_name(repo, path):
    raw = repo.replace("/", "__") + "__" + path.replace("/", "__")
    raw = re.sub(r"[^A-Za-z0-9._-]+", "_", raw)
    if len(raw) > 180:
        h = hashlib.md5(raw.encode()).hexdigest()[:10]
        raw = raw[:160] + "__" + h + Path(path).suffix
    return raw

def search(q, page):
    r = requests.get(
        "https://api.github.com/search/code",
        headers=HEADERS,
        params={"q": q, "per_page": 100, "page": page},
        timeout=30
    )
    if r.status_code != 200:
        print("search fail:", r.status_code, r.text[:180])
        return []
    return r.json().get("items", [])

def download(item):
    repo = item["repository"]["full_name"]
    path = item["path"]

    if Path(path).suffix.lower() not in EXTS:
        return None

    if not says_bill_of_materials(path):
        return None

    out = RAW / safe_name(repo, path)

    if out.exists() and out.stat().st_size > 50:
        print("skip existing:", out)
        return None

    meta = requests.get(item["url"], headers=HEADERS, timeout=30)
    if meta.status_code != 200:
        return None

    url = meta.json().get("download_url")
    if not url:
        return None

    r = requests.get(url, timeout=30)
    if r.status_code == 200 and len(r.content) > 50:
        out.write_bytes(r.content)
        print("saved:", out)
        return str(out), repo, path

    return None

saved = []
seen_keys = set()

for q in QUERIES:
    print("\nQUERY:", q)
    for page in range(1, 11):
        items = search(q, page)
        if not items:
            break

        for item in items:
            key = item["repository"]["full_name"] + "/" + item["path"]
            if key in seen_keys:
                continue
            seen_keys.add(key)

            got = download(item)
            if got:
                saved.append(got)

            time.sleep(0.25)

MPN_NAMES = {
    "mpn", "manufacturer part number", "manufacturer_part_number",
    "mfg part number", "mfg_part_number",
    "part number", "part_number",
    "manufacturer pn", "manufacturerpn",
    "manufacturer no", "manufacturer_no",
    "mfr part number", "mfr_part_number",
    "mfg pn", "mfg_pn"
}

QTY_NAMES = {"qty", "quantity", "qnty", "count", "amount"}

rows = []

for f, repo, path in saved:
    try:
        if f.lower().endswith((".xlsx", ".xls")):
            df = pd.read_excel(f)
        elif f.lower().endswith(".tsv"):
            df = pd.read_csv(f, sep="\t", on_bad_lines="skip", engine="python")
        else:
            df = pd.read_csv(f, on_bad_lines="skip", encoding_errors="ignore", engine="python")

        df.columns = [str(c).strip().lower() for c in df.columns]

        mpn_col = next((c for c in df.columns if c in MPN_NAMES), None)
        qty_col = next((c for c in df.columns if c in QTY_NAMES), None)

        if not mpn_col:
            continue

        for _, r in df.iterrows():
            mpn = str(r.get(mpn_col, "")).strip()
            if mpn and mpn.lower() not in {"nan", "none", "n/a", "na"}:
                rows.append({
                    "mpn": mpn,
                    "quantity": r.get(qty_col, "") if qty_col else "",
                    "repo": repo,
                    "source_file": path,
                    "local_file": f
                })

    except Exception as e:
        print("parse fail:", f, e)

new_rows = pd.DataFrame(rows).drop_duplicates()

new_outfile = OUT / "normalized_bill_of_materials_only_new.csv"
new_rows.to_csv(new_outfile, index=False)

master = OUT / "normalized_bom_lines.csv"
if master.exists():
    old = pd.read_csv(master, on_bad_lines="skip")
    combined = pd.concat([old, new_rows], ignore_index=True).drop_duplicates()
else:
    combined = new_rows

combined_outfile = OUT / "normalized_bom_lines_combined.csv"
combined.to_csv(combined_outfile, index=False)

print("\nDONE")
print("new bill-of-materials files downloaded:", len(saved))
print("new normalized rows:", len(new_rows))
print("new output:", new_outfile)
print("combined output:", combined_outfile)
