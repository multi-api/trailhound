"""Drives a running server through the official meilisearch-python client.

Mirrors tests/sdk/compat.mjs. Start the server with
    trailhound --data-dir ./data-test --addr 127.0.0.1:7711 --master-key test-master-key
and run `python compat.py`.
"""

import os
import sys
import warnings

import meilisearch
from meilisearch.errors import MeilisearchApiError

HOST = os.environ.get("TRAILHOUND_URL", "http://127.0.0.1:7711")
KEY = os.environ.get("TRAILHOUND_KEY", "test-master-key")
UID = "py_books"

client = meilisearch.Client(HOST, KEY)
passed = 0
failures = []
expected = []
first_task = 0  # tasks of earlier runs against the same server are ignored


def check(name, missing=None):
    """Run one check. `missing` names the documented missing feature that
    makes the check fail; such a check must fail, so the list stays honest."""

    def run(fn):
        global passed
        try:
            fn()
        except Exception as e:  # noqa: BLE001
            line = str(e).split("\n")[0][:220]
            if missing:
                expected.append(name)
                print(f"xfail {name} ({missing}) -> {line}")
            else:
                failures.append(name)
                print(f"FAIL  {name} -> {type(e).__name__}: {line}")
        else:
            if missing:
                failures.append(name)
                print(f"FAIL  {name} -> passed, but is listed as missing: {missing}")
            else:
                passed += 1
                print(f"ok    {name}")
        return fn

    return run


def done(info):
    task = client.wait_for_task(info.task_uid)
    assert task.status == "succeeded", task.error
    return task


def rejects(code, fn):
    try:
        fn()
    except MeilisearchApiError as e:
        assert e.code == code, f"expected {code}, got {e.code}"
    else:
        raise AssertionError(f"expected the error {code}")


def ids(result):
    return [h["id"] for h in result["hits"]]


books = [
    {"id": 1, "title": "Cien años de soledad", "author": "Gabriel García Márquez", "year": 1967, "genres": ["novela", "realismo mágico"], "price": 12.5},
    {"id": 2, "title": "La casa de los espíritus", "author": "Isabel Allende", "year": 1982, "genres": ["novela"], "price": 10},
    {"id": 3, "title": "Rayuela", "author": "Julio Cortázar", "year": 1963, "genres": ["novela", "experimental"], "price": 14},
    {"id": 4, "title": "Veinte poemas de amor y una canción desesperada", "author": "Pablo Neruda", "year": 1924, "genres": ["poesía"], "price": 8},
    {"id": 5, "title": "Los detectives salvajes", "author": "Roberto Bolaño", "year": 1998, "genres": ["novela"], "price": 16},
]
index = client.index(UID)


@check("health / is_healthy")
def _():
    assert client.health()["status"] == "available"
    assert client.is_healthy() is True


@check("get_version")
def _():
    assert client.get_version()["pkgVersion"]


@check("delete_index (missing index)")
def _():
    global first_task
    info = client.delete_index(UID)
    first_task = info.task_uid
    task = client.wait_for_task(info.task_uid)
    if task.status == "succeeded":  # left over from an interrupted run
        task = client.wait_for_task(client.delete_index(UID).task_uid)
    assert task.status == "failed" and task.error["code"] == "index_not_found"


@check("create_index + wait_for_task")
def _():
    task = done(client.create_index(UID, {"primaryKey": "id"}))
    assert task.type == "indexCreation"
    assert task.index_uid == UID
    assert task.enqueued_at and task.started_at and task.finished_at and task.duration


@check("get_index / get_raw_index / fetch_info")
def _():
    ix = client.get_index(UID)
    assert ix.primary_key == "id"
    assert ix.created_at and ix.updated_at
    assert client.get_raw_index(UID)["uid"] == UID
    assert client.index(UID).fetch_info().uid == UID


@check("get_indexes / get_raw_indexes")
def _():
    r = client.get_indexes({"limit": 50})
    assert any(i.uid == UID for i in r["results"])
    assert r["total"] >= 1 and r["limit"] == 50 and r["offset"] == 0
    assert any(i["uid"] == UID for i in client.get_raw_indexes({"limit": 50})["results"])


@check("add_documents")
def _():
    task = done(index.add_documents(books))
    assert task.details["indexedDocuments"] == 5
    assert task.details["receivedDocuments"] == 5


@check("add_documents_in_batches")
def _():
    tasks = index.add_documents_in_batches([{"id": 6, "title": "Temporal", "year": 2000}, {"id": 7, "title": "Temporal dos", "year": 2000}], 1)
    assert len(tasks) == 2
    for t in tasks:
        done(t)


@check("add_documents_ndjson / add_documents_json")
def _():
    done(index.add_documents_ndjson(b'{"id": 8, "title": "Linea uno"}\n{"id": 9, "title": "Linea dos"}\n'))
    done(index.add_documents_json(b'[{"id": 10, "title": "Texto json"}]'))
    assert index.get_document(9).title == "Linea dos"
    assert index.get_document(10).title == "Texto json"
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", DeprecationWarning)
        done(index.delete_documents([8, 9, 10]))


@check("add_documents_csv", missing="CSV documents")
def _():
    done(index.add_documents_csv(b"id,title\n11,Desde csv\n"))


@check("update_settings")
def _():
    done(index.update_settings({"filterableAttributes": ["genres", "year", "author"], "sortableAttributes": ["year", "price"], "stopWords": ["de", "la", "los"]}))


@check("get_settings")
def _():
    s = index.get_settings()
    assert s["sortableAttributes"] == ["year", "price"]
    assert isinstance(s["rankingRules"], list)


@check("get/update/reset single settings")
def _():
    done(index.update_synonyms({"libro": ["novela"]}))
    assert index.get_synonyms() == {"libro": ["novela"]}
    done(index.update_typo_tolerance({"minWordSizeForTypos": {"oneTypo": 4}}))
    assert index.get_typo_tolerance().min_word_size_for_typos.one_typo == 4
    done(index.reset_typo_tolerance())
    assert index.get_typo_tolerance().min_word_size_for_typos.one_typo == 5
    assert sorted(index.get_filterable_attributes()) == ["author", "genres", "year"]
    assert index.get_sortable_attributes() == ["year", "price"]
    assert index.get_searchable_attributes() == ["*"]
    assert index.get_displayed_attributes() == ["*"]
    assert index.get_pagination_settings().max_total_hits == 1000
    assert index.get_faceting_settings().max_values_per_facet == 100
    assert index.get_distinct_attribute() is None
    assert sorted(index.get_stop_words()) == ["de", "la", "los"]
    done(index.update_ranking_rules(["words", "typo", "proximity", "attribute", "sort", "exactness"]))
    assert index.get_ranking_rules() == ["words", "typo", "proximity", "attribute", "sort", "exactness"]
    done(index.update_searchable_attributes(["title", "author"]))
    assert index.get_searchable_attributes() == ["title", "author"]
    done(index.reset_searchable_attributes())
    done(index.update_displayed_attributes(["id", "title", "author", "year", "genres", "price"]))
    done(index.reset_displayed_attributes())
    done(index.update_pagination_settings({"maxTotalHits": 500}))
    assert index.get_pagination_settings().max_total_hits == 500
    done(index.reset_pagination_settings())
    done(index.update_faceting_settings({"maxValuesPerFacet": 50}))
    assert index.get_faceting_settings().max_values_per_facet == 50
    done(index.reset_faceting_settings())


@check("search: typo + accents")
def _():
    r = index.search("garsia marques")
    assert r["hits"][0]["id"] == 1
    assert isinstance(r["processingTimeMs"], int)
    assert isinstance(r["estimatedTotalHits"], int)
    assert r["query"] == "garsia marques"


@check("search: filter + sort + facets")
def _():
    r = index.search("", {"filter": ["genres = novela", "year > 1965"], "sort": ["year:desc"], "facets": ["genres"]})
    assert ids(r) == [5, 2, 1]
    assert r["facetDistribution"]["genres"]["novela"] == 3
    r = index.search("", {"filter": 'author = "Isabel Allende" OR year IN [1963, 1998]', "sort": ["price:asc"]})
    assert ids(r) == [2, 3, 5]


@check("search: phrase")
def _():
    assert ids(index.search('"poemas de amor"')) == [4]
    assert len(index.search('"amor poemas"')["hits"]) == 0


@check("search: highlight, crop, retrieve, matches")
def _():
    r = index.search("soledad", {"attributesToRetrieve": ["title"], "attributesToHighlight": ["title"], "highlightPreTag": "<b>", "highlightPostTag": "</b>", "showMatchesPosition": True})
    assert sorted(r["hits"][0].keys()) == ["_formatted", "_matchesPosition", "title"]
    assert r["hits"][0]["_formatted"]["title"] == "Cien años de <b>soledad</b>"
    assert len(r["hits"][0]["_matchesPosition"]["title"]) == 1


@check("search: offset / limit")
def _():
    r = index.search("", {"offset": 1, "limit": 2, "sort": ["year:asc"]})
    assert ids(r) == [3, 1]
    assert r["offset"] == 1 and r["limit"] == 2


@check("search: page / hitsPerPage")
def _():
    r = index.search("", {"page": 2, "hitsPerPage": 2})
    assert len(r["hits"]) == 2
    assert r["totalHits"] == 7 and r["totalPages"] == 4 and r["page"] == 2 and r["hitsPerPage"] == 2


@check("search: synonym")
def _():
    assert len(index.search("libro", {"limit": 20})["hits"]) >= 4


@check("search: bad filter gives a typed error")
def _():
    rejects("invalid_search_filter", lambda: index.search("", {"filter": "title = x"}))


@check("search: distinct", missing="distinctAttribute")
def _():
    # Documents 6 and 7 share a year.
    assert ids(index.search("temporal", {"distinct": "year"})) == [6]


@check("multi_search")
def _():
    r = client.multi_search([{"indexUid": UID, "q": "neruda"}, {"indexUid": UID, "q": "bolaño"}])
    assert len(r["results"]) == 2
    assert r["results"][0]["indexUid"] == UID
    assert r["results"][1]["hits"][0]["id"] == 5


@check("multi_search (federated)", missing="federated search")
def _():
    r = client.multi_search([{"indexUid": UID, "q": "neruda"}], federation={})
    assert len(r["hits"]) == 1


@check("get_document")
def _():
    assert index.get_document(3).title == "Rayuela"
    assert dict(index.get_document(3, {"fields": ["id"]})) == {"id": 3}


@check("get_documents (query + filter)")
def _():
    r = index.get_documents({"limit": 3, "offset": 1, "fields": ["id"]})
    assert len(r.results) == 3 and r.total == 7 and r.offset == 1 and r.limit == 3
    assert dict(r.results[0]) == {"id": 2}
    f = index.get_documents({"filter": 'author = "Isabel Allende"'})
    assert f.results[0].id == 2


@check("update_documents (partial)")
def _():
    done(index.update_documents([{"id": 2, "price": 11.25}]))
    d = index.get_document(2)
    assert d.price == 11.25 and d.author == "Isabel Allende"


@check("delete_document")
def _():
    done(index.delete_document(6))
    rejects("document_not_found", lambda: index.get_document(6))


@check("delete_documents (ids)")
def _():
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", DeprecationWarning)
        task = done(index.delete_documents([7]))
    assert task.details["deletedDocuments"] == 1


@check("delete_documents (filter)")
def _():
    task = done(index.delete_documents(filter="year < 1930"))
    assert task.details["deletedDocuments"] == 1


@check("get_stats / get_all_stats")
def _():
    s = index.get_stats()
    assert s.number_of_documents == 4 and s.is_indexing is False
    assert s.field_distribution.title == 4
    assert client.get_all_stats()["indexes"][UID]["numberOfDocuments"] == 4


@check("get_tasks / get_task")
def _():
    r = client.get_tasks({"indexUids": [UID], "limit": 3})
    assert len(r.results) == 3 and r.results[0].uid > r.results[1].uid
    assert r.next_ is not None and r.from_ == r.results[0].uid
    assert client.get_task(r.results[0].uid).uid == r.results[0].uid
    assert index.get_task(r.results[0].uid).uid == r.results[0].uid
    assert all(t.index_uid == UID for t in index.get_tasks().results)
    # Deleting the missing index at the start is recorded as a failed task, as in Meilisearch.
    failed = client.get_tasks({"statuses": ["failed"], "indexUids": [UID]})
    assert failed.total == len(failed.results)
    assert all(t.status == "failed" for t in failed.results)
    mine = [t for t in failed.results if t.uid >= first_task]
    assert mine and all(t.error["code"] == "index_not_found" for t in mine)
    typed = client.get_tasks({"types": ["documentAdditionOrUpdate"], "indexUids": [UID]})
    assert typed.results and all(t.type == "documentAdditionOrUpdate" for t in typed.results)


@check("get_tasks: uids, reverse, dates")
def _():
    newest = client.get_tasks({"indexUids": [UID], "limit": 3}).results
    picked = client.get_tasks({"uids": [str(newest[0].uid), str(newest[2].uid)]})
    assert [t.uid for t in picked.results] == [newest[0].uid, newest[2].uid] and picked.total == 2
    oldest = client.get_tasks({"indexUids": [UID], "reverse": "true", "from": first_task, "limit": 2})
    assert [t.uid for t in oldest.results] == [first_task, oldest.results[1].uid] and oldest.results[1].uid > first_task
    assert oldest.next_ > oldest.results[1].uid
    assert client.get_tasks({"indexUids": [UID], "afterEnqueuedAt": "2099-01-01T00:00:00Z"}).total == 0
    assert client.get_tasks({"indexUids": [UID], "beforeEnqueuedAt": "2099-01-01", "afterFinishedAt": "2020-01-01T00:00:00+02:00", "limit": 1}).total >= 3
    assert client.get_tasks({"indexUids": [UID], "canceledBy": [str(newest[0].uid)]}).total == 0
    rejects("bad_request", lambda: client.get_tasks({"uids": "abc"}))


@check("failed task carries an error")
def _():
    task = client.wait_for_task(client.create_index(UID).task_uid)
    assert task.status == "failed" and task.error["code"] == "index_already_exists"


@check("get_batches", missing="batches")
def _():
    client.get_batches()


@check("cancel_tasks", missing="task cancellation")
def _():
    client.cancel_tasks({"statuses": ["enqueued"]})


@check("keys: create, get, update, restricted use, delete")
def _():
    k = client.create_key({"name": "solo libros", "actions": ["search"], "indexes": [UID], "expiresAt": None})
    assert k.key and k.uid
    assert k.actions == ["search"] and k.indexes == [UID] and k.expires_at is None
    assert client.get_key(k.uid).name == "solo libros"
    assert client.get_key(k.key).uid == k.uid
    assert client.update_key(k.uid, {"description": "x"}).description == "x"
    assert any(x.uid == k.uid for x in client.get_keys({"limit": 50}).results)
    limited = meilisearch.Client(HOST, k.key)
    assert limited.index(UID).search("rayuela")["hits"][0]["id"] == 3
    rejects("invalid_api_key", lambda: limited.index("products").search("x"))
    rejects("invalid_api_key", lambda: limited.index(UID).add_documents([{"id": 99}]))
    client.delete_key(k.uid)
    rejects("invalid_api_key", lambda: limited.index(UID).search("rayuela"))


# Keys with an expiry are checked in the Go and PHP suites: this client cannot
# parse an `expiresAt` without fractional seconds, which is how Meilisearch and
# Trailhound both write it.


@check("get_primary_key / update index")
def _():
    assert index.get_primary_key() == "id"
    tmp = UID + "_tmp"
    client.wait_for_task(client.delete_index(tmp).task_uid)
    done(client.create_index(tmp))
    assert client.index(tmp).get_primary_key() is None
    done(client.index(tmp).update(primary_key="isbn"))
    assert client.index(tmp).get_primary_key() == "isbn"
    done(client.index(tmp).delete())


@check("swap_indexes", missing="index swaps")
def _():
    done(client.swap_indexes([{"indexes": [UID, UID + "_other"]}]))


@check("delete_all_documents")
def _():
    done(index.delete_all_documents())
    assert index.get_stats().number_of_documents == 0


@check("reset_settings")
def _():
    done(index.reset_settings())
    assert index.get_filterable_attributes() == []


@check("delete_index")
def _():
    done(client.delete_index(UID))
    rejects("index_not_found", lambda: client.get_index(UID))


print(f"\n{passed} passed, {len(failures)} failed, {len(expected)} expected failures (missing features)")
if failures:
    print("failed: " + " | ".join(failures))
    sys.exit(1)
