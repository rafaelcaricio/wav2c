#!/usr/bin/env python3
"""Convert the published AV2 specification HTML into a single Markdown file."""
import argparse
import re
import subprocess
from pathlib import Path

from bs4 import BeautifulSoup, NavigableString, Tag

PLACEHOLDER = "AV2TABLEPLACEHOLDER"


def indent_of(cell):
    span = cell.find("span")
    if span and span.get("style"):
        match = re.search(r"padding-left:\s*([0-9.]+)em", span["style"])
        if match:
            return int(round(float(match.group(1)) * 2))
    return 0


def syntax_table(table):
    lines = []
    name = table.find("th", class_="sdl-syntax-name")
    if name:
        first = name.find(string=True)
        if first:
            lines.append(first.strip())
    for span in table.find_all("span"):
        style = span.get("style") or ""
        match = re.search(r"padding-left:\s*([0-9.]+)em", style)
        indent = int(round(float(match.group(1)) * 2)) if match else 0
        text = span.get_text(" ", strip=True)
        if text:
            lines.append("  " * indent + text)
    return "\n".join(lines)


def plain_table(table):
    caption = table.find("caption")
    header = []
    rows = []
    thead = table.find("thead")
    if thead and thead.find("tr"):
        header = [c.get_text(" ", strip=True) for c in thead.find("tr").find_all(["th", "td"])]
    for tr in table.find_all("tr"):
        if thead and tr in thead.find_all("tr"):
            continue
        cells = [c.get_text(" ", strip=True) for c in tr.find_all(["td", "th"])]
        if cells:
            rows.append(cells)
    if not rows:
        return None
    width = max([len(header)] + [len(r) for r in rows])
    if not header:
        header = [""] * width
    header = header + [""] * (width - len(header))

    def esc(value):
        return value.replace("|", "\\|").replace("\n", " ")

    out = []
    if caption:
        out.append(f"**{caption.get_text(' ', strip=True)}**\n")
    out.append("| " + " | ".join(esc(v) for v in header) + " |")
    out.append("|" + "|".join(["---"] * width) + "|")
    for row in rows:
        row = row + [""] * (width - len(row))
        out.append("| " + " | ".join(esc(v) for v in row[:width]) + " |")
    return "\n".join(out)


def prepare(html):
    soup = BeautifulSoup(html, "lxml")
    for tag in soup(["script", "style", "nav", "header", "footer"]):
        tag.decompose()
    for tag in soup.select(".self-link, .section-toggle, .back-to-toc-wrapper"):
        tag.decompose()

    tables = []
    for table in soup.find_all("table"):
        classes = table.get("class") or []
        if "sdl-syntax-table" in classes:
            text = syntax_table(table)
            if not text:
                continue
            holder = soup.new_tag("p")
            key = f"{PLACEHOLDER}SYNTAX{len(tables)}"
            holder.string = key
            tables.append((key, "```\n" + text + "\n```"))
            table.replace_with(holder)
        else:
            markdown = plain_table(table)
            if markdown is None:
                continue
            key = f"{PLACEHOLDER}{len(tables)}"
            tables.append((key, markdown))
            holder = soup.new_tag("p")
            holder.string = key
            table.replace_with(holder)

    for heading in soup.find_all(["h1", "h2", "h3", "h4", "h5", "h6"]):
        text = heading.get_text(" ", strip=True)
        text = re.sub(r"\s+", " ", text)
        heading.clear()
        heading.append(NavigableString(text))

    return soup, tables


def convert_section(tag, pandoc):
    data = str(tag)
    result = subprocess.run(
        [pandoc, "-f", "html", "-t", "gfm", "--wrap=none"],
        input=data.encode("utf-8"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return result.stdout.decode("utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("html")
    parser.add_argument("output")
    parser.add_argument("--pandoc", default="pandoc")
    args = parser.parse_args()

    soup, tables = prepare(Path(args.html).read_text(encoding="utf-8"))
    headings = soup.find_all("h2")
    parts = []
    for index, heading in enumerate(headings):
        fragment = [str(heading)]
        for sibling in heading.next_siblings:
            if isinstance(sibling, Tag) and sibling.name == "h2":
                break
            if isinstance(sibling, Tag):
                fragment.append(str(sibling))
        markdown = convert_section("".join(fragment), args.pandoc)
        markdown = re.sub(r"^▼ Show/Hide Section .*$", "", markdown, flags=re.M)
        markdown = re.sub(r"(?m)^\s*</?div[^>]*>\s*$", "", markdown)
        markdown = re.sub(
            r"</?(?:span|a|dfn|abbr|sup|sub|c-[a-z]+)\b[^>]*>", "", markdown
        )
        for key, table in tables:
            markdown = markdown.replace(key, "\n" + table + "\n")
        markdown = re.sub(r"\n{4,}", "\n\n\n", markdown)
        parts.append(markdown.strip())

    header = (
        "# AV2 Bitstream & Decoding Process Specification\n\n"
        "Version v1.0.0 (Final Deliverable, 28 May 2026).\n\n"
        "Converted from the published HTML specification at\n"
        "https://av2.aomedia.org/v1.0.0/index.html for local reference.\n"
        "Normative text is the published specification; this Markdown copy is\n"
        "generated and may lose some table or figure formatting.\n"
    )
    Path(args.output).write_text(header + "\n\n" + "\n\n".join(parts) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
