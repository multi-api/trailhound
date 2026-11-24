#!/usr/bin/env python3
"""Does Trailhound rank like Meilisearch?

For every query, take Trailhound's top hits and ask Meilisearch to score
exactly those documents (same query, filtered to their ids). If the
per-rule scores match the ones of Meilisearch's own top hits, position by
position, the two rankings are equivalent up to ties.

usage: ranking_check.py TRAILHOUND_URL MEILI_URL KEY [INDEX]
Both servers must hold the same documents, with `id` filterable.
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


def main():
    same = 0
    for q in QUERIES:
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
        ok = got == ref and totals
        same += ok
        first_diff = next((i for i, (a, b) in enumerate(zip(got, ref)) if a != b), min(len(got), len(ref)))
        print("%-4s ids=%-5s totals=%-5s  %s" % ("OK" if ok else "DIFF", "same" if exact_ids else "ties" if ok else "-", totals, json.dumps(q, ensure_ascii=False)))
        if not ok and first_diff < max(len(got), len(ref)):
            print("       first difference at rank %d" % (first_diff + 1))
            print("       meilisearch top:   %s" % (ref[first_diff] if first_diff < len(ref) else "(none)"))
            print("       trailhound's doc:  %s" % (got[first_diff] if first_diff < len(got) else "(none)"))
    print("\n%d of %d queries rank the same as Meilisearch (up to ties)" % (same, len(QUERIES)))


if __name__ == "__main__":
    main()
