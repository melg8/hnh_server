#!/usr/bin/env python3
"""Fetch RoB Legacy pages and extract recipe-relevant sections.

Usage: python3 scripts/s79_rob_fetch.py
Writes /tmp/rob_pages.json: {url: {"objects": ..., "acquire": ...}}
"""
import json
import re
import subprocess
import sys

PAGES = [
    ("apple_pie", "https://ringofbrodgar.com/wiki/Legacy:Apple_Pie"),
    ("blueberry_pie", "https://ringofbrodgar.com/wiki/Legacy:Blueberry_Pie"),
    ("carrot_cake", "https://ringofbrodgar.com/wiki/Legacy:Carrot_Cake"),
    ("honey_bun", "https://ringofbrodgar.com/wiki/Legacy:Honey_Bun"),
    ("raisin_bc", "https://ringofbrodgar.com/wiki/Legacy:Raisin_Butter-Cake"),
    ("rob_baking_item", "https://ringofbrodgar.com/wiki/Legacy:Ring_of_Brodgar_(Baking)"),
    ("pirozhki", "https://ringofbrodgar.com/wiki/Legacy:Chantrelle_%26_Onion_Pirozhki"),
    ("jar", "https://ringofbrodgar.com/wiki/Legacy:Jar"),
    ("mug", "https://ringofbrodgar.com/wiki/Legacy:Clay_Mug"),
    ("teapot", "https://ringofbrodgar.com/wiki/Legacy:Teapot"),
    ("treepot", "https://ringofbrodgar.com/wiki/Legacy:Treeplanter%27s_Pot"),
    ("kiln", "https://ringofbrodgar.com/wiki/Legacy:Kiln"),
]


def clean(html):
    html = re.sub(r"<script[^>]*>[\s\S]*?</script>", " ", html)
    html = re.sub(r"<style[^>]*>[\s\S]*?</style>", " ", html)
    text = re.sub(r"<[^>]+>", " ", html)
    return re.sub(r"\s+", " ", text)


out = {}
for key, url in PAGES:
    tmp = f"/tmp/rob_{key}.json"
    try:
        subprocess.run(
            ["z-ai", "function", "-n", "page_reader", "-a",
             json.dumps({"url": url}), "-o", tmp],
            check=True, capture_output=True, timeout=90,
        )
        d = json.load(open(tmp))
        h = d.get("html") or (d.get("data") or {}).get("html") or ""
        text = clean(h)
        objects = ""
        m = re.search(r"Object\(s\) Required ([^P]{0,160}?) Produced", text)
        if m:
            objects = m.group(1).strip()
        acquire = ""
        m2 = re.search(r"How to Acquire([\s\S]{0,700}?) Retrieved from", text)
        if m2:
            acquire = m2.group(1).strip()
        out[key] = {"objects": objects, "acquire": acquire, "len": len(text)}
        print(key, "OK objects=", objects[:120])
    except Exception as e:  # noqa: BLE001 - one-off probe, report and continue
        out[key] = {"error": str(e)[:200]}
        print(key, "FAIL", str(e)[:120])

json.dump(out, open("/tmp/rob_pages.json", "w"), indent=1)
print("saved /tmp/rob_pages.json")
