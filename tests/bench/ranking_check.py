#!/usr/bin/env python3
"""Does Trailhound rank like Meilisearch?

For every query, take Trailhound's top hits and ask Meilisearch to score
exactly those documents (same query, filtered to their ids). If the
per-rule scores match the ones of Meilisearch's own top hits, position by
position, the two rankings are equivalent up to ties.

usage: ranking_check.py TRAILHOUND_URL MEILI_URL KEY [INDEX] [QUERIES_FILE]
Both servers must hold the same documents, with `id` filterable.
QUERIES_FILE is a JSON list of search requests (wiki.py writes one); without
it, the queries below, which fit the documents of gen.js.
"""
import json
import sys
import urllib.request

TH, MEILI, KEY = sys.argv[1], sys.argv[2], sys.argv[3]
INDEX = sys.argv[4] if len(sys.argv) > 4 else "bench"
N = 10

QUERIES = [
    {"q": "motor"},
    {"q": "motor búsqueda"},
    {"q": "búsqueda motor"},
    {"q": "servidr rapdo"},
    {"q": "compre"},
    {"q": "portátil solar eléctrico manual", "matchingStrategy": "all"},
    {"q": "portátil solar eléctrico manual"},
    {"q": "casa 12345"},
    {"q": "garantía", "filter": "stock > 400"},
    {"q": "descuento envío", "sort": ["created:desc"]},
    {"q": "descuento envío"},
    {"q": "precio calidad entrega"},
    {"q": "azul rojo verde negro blanco"},
    {"q": "factura producto prec"},
    {"q": "montaña río ciud"},
    {"q": "digital", "sort": ["price:asc"]},
    {"q": "seguro compresion texto", "matchingStrategy": "all"},
    {"q": "automatico electrico", "filter": "category = Hogar"},
    {"q": "memoria disco servidor cliente", "sort": ["price:desc"]},
    {"q": "tecnico"},
    {"q": "madera metal 777"},
    {"q": "garantia soporte tecnico", "matchingStrategy": "all"},
]


def post(base, body):
    req = urllib.request.Request(base + "/indexes/%s/search" % INDEX, json.dumps(body).encode(), {"Authorization": "Bearer " + KEY, "Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req))


def detail(hit):
    """A hit's per-rule scores, without the fields that only say which rule ran when."""
    out = {}
    for rule, d in hit["_rankingScoreDetails"].items():
        out[rule] = {k: v for k, v in d.items() if k not in ("order",)}
    return json.dumps(out, sort_keys=True, ensure_ascii=False)


RULES = ["words", "typo", "proximity", "attributeRank", "sort", "wordPosition", "exactness"]


def first_rule(a, b):
    """The first ranking rule, in the default order, on which two hits differ."""
    a, b = json.loads(a), json.loads(b)
    for rule in RULES:
        keys = [k for k in set(a) | set(b) if k == rule or (rule == "sort" and ":" in k)]
        if any(a.get(k) != b.get(k) for k in keys):
            return rule
    return "other"


def main():
    same, same_top, same_total, by_kind, by_rule = 0, 0, 0, {}, {}
    queries = json.load(open(sys.argv[5], encoding="utf-8")) if len(sys.argv) > 5 else QUERIES
    for q in queries:
        kind = q.pop("kind", None)
        base = dict(q, limit=N, attributesToRetrieve=["id"])
        ours = post(TH, base)
        theirs = post(MEILI, dict(base, showRankingScoreDetails=True))
        ids = [h["id"] for h in ours["hits"]]
        ref = [detail(h) for h in theirs["hits"]]
        if ids:
            flt = "id IN [%s]" % ", ".join(map(str, ids))
            scored = post(MEILI, dict(base, showRankingScoreDetails=True, filter=[flt] + ([q["filter"]] if "filter" in q else [])))
            got = [detail(h) for h in scored["hits"]]
        else:
            got = []
        totals = ours["estimatedTotalHits"] == theirs["estimatedTotalHits"]
        exact_ids = ids == [h["id"] for h in theirs["hits"]]
        top = got == ref
        ok = top and totals
        same += ok
        same_top += top
        same_total += totals
        tally = by_kind.setdefault(kind, [0, 0, 0, 0])
        tally[0] += ok
        tally[1] += ok and exact_ids
        tally[2] += 1
        tally[3] += top
        first_diff = next((i for i, (a, b) in enumerate(zip(got, ref)) if a != b), min(len(got), len(ref)))
        print("%-4s ids=%-5s totals=%-5s  %s" % ("OK" if ok else "DIFF", "same" if exact_ids else "ties" if top else "-", totals, json.dumps(q, ensure_ascii=False)))
        if not totals:
            print("       matches: trailhound %d, meilisearch %d" % (ours["estimatedTotalHits"], theirs["estimatedTotalHits"]))
        if not top:
            a = got[first_diff] if first_diff < len(got) else None
            b = ref[first_diff] if first_diff < len(ref) else None
            rule = first_rule(a, b) if a and b else "fewer hits"
            by_rule[rule] = by_rule.get(rule, 0) + 1
            print("       first difference at rank %d, in %s" % (first_diff + 1, rule))
            print("       meilisearch top:   %s" % (b or "(none)"))
            print("       trailhound's doc:  %s" % (a or "(none)"))
    n = len(queries)
    print("\n%d of %d queries rank the same as Meilisearch (up to ties) and count the same matches" % (same, n))
    print("%d of %d have the same top %d up to ties; %d of %d count the same matches" % (same_top, n, N, same_total, n))
    print("%d of them with the same ids in the same order" % sum(t[1] for t in by_kind.values()))
    if by_rule:
        print("first rule that differs: " + ", ".join("%s %d" % kv for kv in sorted(by_rule.items(), key=lambda kv: -kv[1])))
    if len(by_kind) > 1:
        for kind, (ok, exact, count, top) in by_kind.items():
            print("  %-7s %d of %d the same (top %d alone: %d), %d with the same ids" % (kind, ok, count, N, top, exact))


if __name__ == "__main__":
    main()
