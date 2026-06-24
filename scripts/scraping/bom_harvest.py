import os, time, requests, pandas as pd
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
    'filename:BOM.csv',
    'filename:bom.csv',
    '"Manufacturer Part Number" extension:csv',
    '"Mfg Part Number" extension:csv',
    '"Designator" "Qty" "MPN" extension:csv',
    '"bill of materials" extension:csv'
]

def search(q, page):
    r = requests.get(
        "https://api.github.com/search/code",
        headers=HEADERS,
        params={"q": q, "per_page": 100, "page": page},
        timeout=30
    )
    if r.status_code != 200:
        print("Search failed:", r.status_code, r.text[:250])
        return []
    return r.json().get("items", [])

def download(item):
    meta = requests.get(item["url"], headers=HEADERS, timeout=30)
    if meta.status_code != 200:
        return None

    j = meta.json()
    download_url = j.get("download_url")
    if not download_url:
        return None

    repo = item["repository"]["full_name"]
    path = item["path"]
    name = repo.replace("/", "__") + "__" + Path(path).name
    out = RAW / name

    r = requests.get(download_url, timeout=30)
    if r.status_code == 200 and len(r.content) > 50:
        out.write_bytes(r.content)
        print("saved", out)
        return str(out), repo, path

    return None

saved = []
seen = set()

for q in QUERIES:
    print("\nQUERY:", q)
    for page in range(1, 6):
        for item in search(q, page):
            key = item["repository"]["full_name"] + "/" + item["path"]
            if key in seen:
                continue
            seen.add(key)

            got = download(item)
            if got:
                saved.append(got)

            time.sleep(0.25)

MPN_NAMES = {
    "mpn", "manufacturer part number", "manufacturer_part_number",
    "mfg part number", "mfg_part_number", "part number",
    "part_number", "manufacturer pn", "manufacturerpn"
}

QTY_NAMES = {"qty", "quantity", "qnty", "count"}

rows = []

for f, repo, path in saved:
    try:
        df = pd.read_csv(f, on_bad_lines="skip", encoding_errors="ignore")
        df.columns = [str(c).strip().lower() for c in df.columns]

        mpn_col = next((c for c in df.columns if c in MPN_NAMES), None)
        qty_col = next((c for c in df.columns if c in QTY_NAMES), None)

        if not mpn_col:
            continue

        for _, r in df.iterrows():
            mpn = str(r.get(mpn_col, "")).strip()
            if mpn and mpn.lower() not in {"nan", "none", "n/a"}:
                rows.append({
                    "mpn": mpn,
                    "quantity": r.get(qty_col, "") if qty_col else "",
                    "repo": repo,
                    "source_file": path
                })

    except Exception as e:
        print("parse fail:", f, e)

out = pd.DataFrame(rows).drop_duplicates()
outfile = OUT / "normalized_bom_lines.csv"
out.to_csv(outfile, index=False)

print("\nDONE")
print("downloaded files:", len(saved))
print("normalized bom rows:", len(out))
print("output:", outfile)
