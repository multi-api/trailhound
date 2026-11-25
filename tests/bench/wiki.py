#!/usr/bin/env python3
"""English Wikipedia as benchmark data.

  wiki.py convert OUT.ndjson MAX_CHARS LIMIT FILE.parquet...
      One document per article, in the order of the files:
      {"id": page id, "title", "url", "text", "length"}.
      `text` is the opening of the article: whole paragraphs up to MAX_CHARS
      characters (0 keeps the whole article). `length` is the size of the
      whole article in characters. LIMIT stops after that many documents
      (0 = all). Needs pyarrow.

  wiki.py queries DOCS.ndjson OUT.json [SEED] [PER_KIND]
      Derives search requests from the documents themselves, the same ones
      for the same file and seed. Standard library only.
"""
import json
import random
import re
import sys

WORD = re.compile(r"[^\W\d_]+")


def lead(text, max_chars):
    if max_chars <= 0 or len(text) <= max_chars:
        return text
    out, n = [], 0
    for p in text.split("\n\n"):
        if out and n + len(p) > max_chars:
            break
        out.append(p)
        n += len(p) + 2
    s = "\n\n".join(out)
    if len(s) > max_chars:  # a first paragraph longer than the limit: cut at a space
        cut = s.rfind(" ", 0, max_chars)
        s = s[: cut if cut > 0 else max_chars]
    return s


def convert(out_path, max_chars, limit, files):
    import pyarrow.parquet as pq

    n = 0
    with open(out_path, "w", encoding="utf-8") as out:
        for path in files:
            for batch in pq.ParquetFile(path).iter_batches(batch_size=2000, columns=["id", "url", "title", "text"]):
                for row in batch.to_pylist():
                    text = row["text"] or ""
                    doc = {"id": int(row["id"]), "title": row["title"], "url": row["url"], "text": lead(text, max_chars), "length": len(text)}
                    out.write(json.dumps(doc, ensure_ascii=False) + "\n")
                    n += 1
                    if n == limit:
                        print("%d documents" % n)
                        return
    print("%d documents" % n)


def adjacent(text, count, min_len):
    """Runs of `count` words separated by single spaces, lowercased."""
    runs, cur, end = [], [], -1
    for m in WORD.finditer(text):
        if cur and text[end:m.start()] != " ":
            cur = []
        cur.append(m.group().lower())
        end = m.end()
        if len(cur) >= count and all(len(w) >= min_len for w in cur[-count:]):
            runs.append(cur[-count:])
    return runs


def typo(rnd, word):
    """One edit that is not in the first letter: drop, swap or replace."""
    while True:
        i = rnd.randrange(1, len(word))
        op = rnd.randrange(3)
        if op == 0:
            out = word[:i] + word[i + 1:]
        elif op == 1 and i + 1 < len(word):
            out = word[:i] + word[i + 1] + word[i] + word[i + 2:]
        else:
            out = word[:i] + rnd.choice("abcdefghijklmnopqrstuvwxyz") + word[i + 1:]
        if out != word:
            return out


def queries(docs_path, out_path, seed, per_kind):
    with open(docs_path, "rb") as f:
        total = sum(chunk.count(b"\n") for chunk in iter(lambda: f.read(1 << 24), b""))
    step = max(1, total // 50000)
    sample = []
    with open(docs_path, encoding="utf-8") as f:
        for i, line in enumerate(f):
            if i % step == 0:
                sample.append(json.loads(line))
    rnd = random.Random(seed)
    lengths = sorted(d["length"] for d in sample)
    quantile = lambda q: lengths[min(len(lengths) - 1, int(len(lengths) * q))]
    out = []

    def make(kind, build):
        made = 0
        while made < per_kind:
            body = build(rnd.choice(sample))
            if body and dict(kind=kind, **body) not in out:
                out.append(dict(kind=kind, **body))
                made += 1

    def title_words(d, min_len=1):
        words = d["title"].lower().split()
        return words if words and all(w.isalpha() and w.isascii() and len(w) >= min_len for w in words) else []

    def word(d):
        words = [w for w in title_words(d) if len(w) >= 4]
        return rnd.choice(words) if words else None

    def one_word(d):
        w = word(d)
        return {"q": w} if w else None

    def whole_title(d):
        words = title_words(d)
        return {"q": " ".join(words)} if 2 <= len(words) <= 4 else None

    def text_words(d):
        runs = adjacent(d["text"], rnd.choice((2, 3)), 2)
        return {"q": " ".join(rnd.choice(runs))} if runs else None

    def misspelt(d):
        words = title_words(d)
        long = [i for i, w in enumerate(words) if len(w) >= 6]
        if not long:
            return None
        i = rnd.choice(long)
        words[i] = typo(rnd, words[i])
        return {"q": " ".join(words[:3])}

    def prefix(d):
        words = title_words(d)[:3]
        if not words or len(words[-1]) < 5:
            return None
        words[-1] = words[-1][: rnd.choice((3, 4))]
        return {"q": " ".join(words)}

    def phrase(d):
        runs = adjacent(d["text"], 2, 3)
        return {"q": '"%s"' % " ".join(rnd.choice(runs))} if runs else None

    def all_words(d):
        words = sorted({w for w in WORD.findall(d["text"].lower())[:80] if len(w) >= 4})
        return {"q": " ".join(rnd.sample(words, 3)), "matchingStrategy": "all"} if len(words) >= 3 else None

    def filtered(d):
        w = word(d)
        flt = rnd.choice(("length > %d" % quantile(0.9), "length < %d" % quantile(0.25), "length %d TO %d" % (quantile(0.5), quantile(0.75))))
        return {"q": w, "filter": flt} if w else None

    def sorted_(d):
        w = word(d)
        return {"q": w, "sort": [rnd.choice(("length:desc", "length:asc", "title:asc"))]} if w else None

    def browse(d):
        lo = rnd.randrange(0, 99) / 100
        return {"q": "", "filter": "length >= %d AND length < %d" % (quantile(lo), quantile(lo + rnd.choice((0.01, 0.05, 0.2)))), "sort": [rnd.choice(("length:asc", "title:asc"))]}

    make("word", one_word)
    make("title", whole_title)
    make("words", text_words)
    make("typo", misspelt)
    make("prefix", prefix)
    make("phrase", phrase)
    make("all", all_words)
    make("filter", filtered)
    make("sort", sorted_)
    make("browse", browse)
    with open(out_path, "w", encoding="utf-8") as f:
        f.write("[\n" + ",\n".join(json.dumps(q, ensure_ascii=False) for q in out) + "\n]\n")
    print("%d queries from a sample of %d of %d documents" % (len(out), len(sample), total))


if __name__ == "__main__":
    a = sys.argv
    if len(a) >= 6 and a[1] == "convert":
        convert(a[2], int(a[3]), int(a[4]), a[5:])
    elif len(a) >= 4 and a[1] == "queries":
        queries(a[2], a[3], int(a[4]) if len(a) > 4 else 42, int(a[5]) if len(a) > 5 else 10)
    else:
        sys.exit(__doc__)
