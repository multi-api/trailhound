// Drives a running server through the official meilisearch-go client.
//
// Mirrors tests/sdk/compat.mjs. Start the server with
//
//	trailhound --data-dir ./data-test --addr 127.0.0.1:7711 --master-key test-master-key
//
// and run `go run .`.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"reflect"
	"sort"
	"strings"
	"time"

	"github.com/meilisearch/meilisearch-go"
)

const uid = "go_books"

var (
	host     = env("TRAILHOUND_URL", "http://127.0.0.1:7711")
	apiKey   = env("TRAILHOUND_KEY", "test-master-key")
	ctx      = context.Background()
	client   = meilisearch.New(host, meilisearch.WithAPIKey(apiKey))
	index    = client.Index(uid)
	passed   int
	failures []string
	expected []string
	// Tasks of earlier runs against the same server are ignored.
	firstTask int64
)

func env(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}

type doc = map[string]any

func firstLine(v any) string {
	s := strings.SplitN(fmt.Sprint(v), "\n", 2)[0]
	if len(s) > 220 {
		s = s[:220]
	}
	return s
}

// run executes one check. `missing` names the documented missing feature that
// makes the check fail; such a check must fail, so the list stays honest.
func run(name, missing string, fn func()) {
	var failure any
	func() {
		defer func() { failure = recover() }()
		fn()
	}()
	switch {
	case failure != nil && missing != "":
		expected = append(expected, name)
		fmt.Printf("xfail %s (%s) -> %s\n", name, missing, firstLine(failure))
	case failure != nil:
		failures = append(failures, name)
		fmt.Printf("FAIL  %s -> %s\n", name, firstLine(failure))
	case missing != "":
		failures = append(failures, name)
		fmt.Printf("FAIL  %s -> passed, but is listed as missing: %s\n", name, missing)
	default:
		passed++
		fmt.Printf("ok    %s\n", name)
	}
}

func check(name string, fn func())                 { run(name, "", fn) }
func checkMissing(name, missing string, fn func()) { run(name, missing, fn) }

func ok(err error) {
	if err != nil {
		panic(err)
	}
}

func must[T any](v T, err error) T {
	ok(err)
	return v
}

func truthy(cond bool, msg string, args ...any) {
	if !cond {
		panic(fmt.Sprintf(msg, args...))
	}
}

// eq compares through JSON, so numbers and nested values compare by value.
func eq(got, want any) {
	g, w := must(json.Marshal(got)), must(json.Marshal(want))
	var gv, wv any
	ok(json.Unmarshal(g, &gv))
	ok(json.Unmarshal(w, &wv))
	if !reflect.DeepEqual(gv, wv) {
		panic(fmt.Sprintf("got %s, want %s", g, w))
	}
}

func wait(info *meilisearch.TaskInfo, err error) *meilisearch.Task {
	ok(err)
	return must(client.WaitForTask(ctx, info.TaskUID, 20*time.Millisecond))
}

func done(info *meilisearch.TaskInfo, err error) *meilisearch.Task {
	task := wait(info, err)
	truthy(task.Status == meilisearch.TaskStatusSucceeded, "task %d: %s %s", task.UID, task.Status, task.Error.Message)
	return task
}

func rejects(code string, err error) {
	var apiErr *meilisearch.Error
	truthy(err != nil, "expected the error %s", code)
	truthy(errors.As(err, &apiErr), "not an API error: %v", err)
	truthy(string(apiErr.APIError.Code) == code, "expected %s, got %s", code, apiErr.APIError.Code)
}

func hits(res *meilisearch.SearchResponse) []doc {
	var out []doc
	ok(res.Hits.DecodeInto(&out))
	return out
}

func ids(res *meilisearch.SearchResponse) []any {
	out := []any{}
	for _, h := range hits(res) {
		out = append(out, h["id"])
	}
	return out
}

func search(q string, req *meilisearch.SearchRequest) *meilisearch.SearchResponse {
	if req == nil {
		req = &meilisearch.SearchRequest{}
	}
	return must(index.Search(ctx, q, req))
}

func sorted(v []string) []string {
	out := append([]string{}, v...)
	sort.Strings(out)
	return out
}

var books = []doc{
	{"id": 1, "title": "Cien años de soledad", "author": "Gabriel García Márquez", "year": 1967, "genres": []string{"novela", "realismo mágico"}, "price": 12.5},
	{"id": 2, "title": "La casa de los espíritus", "author": "Isabel Allende", "year": 1982, "genres": []string{"novela"}, "price": 10},
	{"id": 3, "title": "Rayuela", "author": "Julio Cortázar", "year": 1963, "genres": []string{"novela", "experimental"}, "price": 14},
	{"id": 4, "title": "Veinte poemas de amor y una canción desesperada", "author": "Pablo Neruda", "year": 1924, "genres": []string{"poesía"}, "price": 8},
	{"id": 5, "title": "Los detectives salvajes", "author": "Roberto Bolaño", "year": 1998, "genres": []string{"novela"}, "price": 16},
}

func main() {
	check("Health / IsHealthy", func() {
		eq(must(client.Health(ctx)).Status, "available")
		truthy(client.IsHealthy(), "not healthy")
	})
	check("Version", func() { truthy(must(client.Version(ctx)).PkgVersion != "", "no pkgVersion") })
	check("DeleteIndex (missing index)", func() {
		info := must(client.DeleteIndex(ctx, uid))
		firstTask = info.TaskUID
		task := wait(info, nil)
		if task.Status == meilisearch.TaskStatusSucceeded { // left over from an interrupted run
			task = wait(client.DeleteIndex(ctx, uid))
		}
		eq(task.Status, "failed")
		eq(task.Error.Code, "index_not_found")
	})
	check("CreateIndex + WaitForTask", func() {
		task := done(client.CreateIndex(ctx, &meilisearch.IndexConfig{Uid: uid, PrimaryKey: "id"}))
		eq(task.Type, "indexCreation")
		eq(task.IndexUID, uid)
		truthy(!task.EnqueuedAt.IsZero() && !task.StartedAt.IsZero() && !task.FinishedAt.IsZero() && task.Duration != "", "missing task times")
	})
	check("GetIndex / GetRawIndex / FetchInfo", func() {
		ix := must(client.GetIndex(ctx, uid))
		eq(ix.PrimaryKey, "id")
		truthy(!ix.CreatedAt.IsZero() && !ix.UpdatedAt.IsZero(), "missing index times")
		eq(must(client.GetRawIndex(ctx, uid))["uid"], uid)
		eq(must(index.FetchInfo(ctx)).UID, uid)
	})
	check("ListIndexes / GetRawIndexes", func() {
		r := must(client.ListIndexes(ctx, &meilisearch.IndexesQuery{Limit: 50}))
		found := false
		for _, ix := range r.Results {
			found = found || ix.UID == uid
		}
		truthy(found, "index not listed")
		truthy(r.Total >= 1 && r.Limit == 50 && r.Offset == 0, "bad page: %+v", r)
		truthy(len(must(client.GetRawIndexes(ctx, &meilisearch.IndexesQuery{Limit: 50}))["results"].([]any)) >= 1, "no raw indexes")
	})
	check("AddDocuments", func() {
		task := done(index.AddDocuments(ctx, books, nil))
		eq(task.Details.IndexedDocuments, 5)
		eq(task.Details.ReceivedDocuments, 5)
	})
	check("AddDocumentsInBatches", func() {
		// Documents 6 and 7 share a year, for the distinct check.
		tasks := must(index.AddDocumentsInBatches(ctx, []doc{{"id": 6, "title": "Temporal", "year": 2000}, {"id": 7, "title": "Temporal dos", "year": 2000}}, 1, nil))
		eq(len(tasks), 2)
		for i := range tasks {
			done(&tasks[i], nil)
		}
	})
	check("AddDocumentsNdjson", func() {
		pk := "id"
		done(index.AddDocumentsNdjson(ctx, []byte("{\"id\": 8, \"title\": \"Linea uno\"}\n{\"id\": 9, \"title\": \"Linea dos\"}\n"), &meilisearch.DocumentOptions{PrimaryKey: &pk}))
		var d doc
		ok(index.GetDocument(ctx, "9", nil, &d))
		eq(d["title"], "Linea dos")
		done(index.DeleteDocuments(ctx, []string{"8", "9"}, nil))
	})
	checkMissing("AddDocumentsCsv", "CSV documents", func() {
		done(index.AddDocumentsCsv(ctx, []byte("id,title\n11,Desde csv\n"), nil))
	})
	check("UpdateSettings", func() {
		done(index.UpdateSettings(ctx, &meilisearch.Settings{
			FilterableAttributes: []string{"genres", "year", "author"},
			SortableAttributes:   []string{"year", "price"},
			StopWords:            []string{"de", "la", "los"},
		}))
	})
	check("GetSettings", func() {
		s := must(index.GetSettings(ctx))
		eq(s.SortableAttributes, []string{"year", "price"})
		truthy(len(s.RankingRules) > 0, "no ranking rules")
		eq(s.Pagination.MaxTotalHits, 1000)
		eq(s.TypoTolerance.MinWordSizeForTypos.TwoTypos, 9)
	})
	check("get/update/reset single settings", func() {
		done(index.UpdateSynonyms(ctx, &map[string][]string{"libro": {"novela"}}))
		eq(must(index.GetSynonyms(ctx)), map[string][]string{"libro": {"novela"}})
		done(index.UpdateTypoTolerance(ctx, &meilisearch.TypoTolerance{Enabled: true, MinWordSizeForTypos: meilisearch.MinWordSizeForTypos{OneTypo: 4, TwoTypos: 9}}))
		eq(must(index.GetTypoTolerance(ctx)).MinWordSizeForTypos.OneTypo, 4)
		done(index.ResetTypoTolerance(ctx))
		eq(must(index.GetTypoTolerance(ctx)).MinWordSizeForTypos.OneTypo, 5)
		filterable := []string{}
		for _, f := range *must(index.GetFilterableAttributes(ctx)) {
			filterable = append(filterable, f.(string))
		}
		eq(sorted(filterable), []string{"author", "genres", "year"})
		eq(must(index.GetSortableAttributes(ctx)), []string{"year", "price"})
		eq(must(index.GetSearchableAttributes(ctx)), []string{"*"})
		eq(must(index.GetDisplayedAttributes(ctx)), []string{"*"})
		eq(must(index.GetPagination(ctx)).MaxTotalHits, 1000)
		eq(must(index.GetFaceting(ctx)).MaxValuesPerFacet, 100)
		if d := must(index.GetDistinctAttribute(ctx)); d != nil {
			eq(*d, "") // how this client reports a null distinct attribute
		}
		eq(sorted(*must(index.GetStopWords(ctx))), []string{"de", "la", "los"})
		rules := []string{"words", "typo", "proximity", "attribute", "sort", "exactness"}
		done(index.UpdateRankingRules(ctx, &rules))
		eq(must(index.GetRankingRules(ctx)), rules)
		searchable := []string{"title", "author"}
		done(index.UpdateSearchableAttributes(ctx, &searchable))
		eq(must(index.GetSearchableAttributes(ctx)), searchable)
		done(index.ResetSearchableAttributes(ctx))
		displayed := []string{"id", "title", "author", "year", "genres", "price"}
		done(index.UpdateDisplayedAttributes(ctx, &displayed))
		done(index.ResetDisplayedAttributes(ctx))
		done(index.UpdatePagination(ctx, &meilisearch.Pagination{MaxTotalHits: 500}))
		eq(must(index.GetPagination(ctx)).MaxTotalHits, 500)
		done(index.ResetPagination(ctx))
		done(index.UpdateFaceting(ctx, &meilisearch.Faceting{MaxValuesPerFacet: 50}))
		eq(must(index.GetFaceting(ctx)).MaxValuesPerFacet, 50)
		done(index.ResetFaceting(ctx))
	})
	check("Search: typo + accents", func() {
		r := search("garsia marques", nil)
		eq(ids(r)[0], 1)
		eq(r.Query, "garsia marques")
		eq(r.EstimatedTotalHits, 1)
		eq(r.Limit, 20)
	})
	check("Search: filter + sort + facets", func() {
		r := search("", &meilisearch.SearchRequest{Filter: []string{"genres = novela", "year > 1965"}, Sort: []string{"year:desc"}, Facets: []string{"genres"}})
		eq(ids(r), []int{5, 2, 1})
		var facets map[string]map[string]int
		ok(json.Unmarshal(r.FacetDistribution, &facets))
		eq(facets["genres"]["novela"], 3)
		r = search("", &meilisearch.SearchRequest{Filter: `author = "Isabel Allende" OR year IN [1963, 1998]`, Sort: []string{"price:asc"}})
		eq(ids(r), []int{2, 3, 5})
	})
	check("Search: phrase", func() {
		eq(ids(search(`"poemas de amor"`, nil)), []int{4})
		eq(len(search(`"amor poemas"`, nil).Hits), 0)
	})
	check("Search: highlight, crop, retrieve, matches", func() {
		r := search("soledad", &meilisearch.SearchRequest{AttributesToRetrieve: []string{"title"}, AttributesToHighlight: []string{"title"}, HighlightPreTag: "<b>", HighlightPostTag: "</b>", ShowMatchesPosition: true})
		hit := hits(r)[0]
		keys := []string{}
		for k := range hit {
			keys = append(keys, k)
		}
		eq(sorted(keys), []string{"_formatted", "_matchesPosition", "title"})
		eq(hit["_formatted"].(doc)["title"], "Cien años de <b>soledad</b>")
		eq(len(hit["_matchesPosition"].(doc)["title"].([]any)), 1)
	})
	check("Search: offset / limit", func() {
		r := search("", &meilisearch.SearchRequest{Offset: 1, Limit: 2, Sort: []string{"year:asc"}})
		eq(ids(r), []int{3, 1})
		eq(r.Offset, 1)
		eq(r.Limit, 2)
	})
	check("Search: page / hitsPerPage", func() {
		perPage := int64(2)
		r := search("", &meilisearch.SearchRequest{Page: 2, HitsPerPage: &perPage})
		eq(len(r.Hits), 2)
		eq([]int64{r.TotalHits, r.TotalPages, r.Page, r.HitsPerPage}, []int{7, 4, 2, 2})
	})
	check("Search: synonym", func() {
		truthy(len(search("libro", &meilisearch.SearchRequest{Limit: 20}).Hits) >= 4, "synonym not applied")
	})
	check("Search: bad filter gives a typed error", func() {
		_, err := index.Search(ctx, "", &meilisearch.SearchRequest{Filter: "title = x"})
		rejects("invalid_search_filter", err)
	})
	check("SearchRaw", func() {
		var r struct {
			Hits []doc `json:"hits"`
		}
		ok(json.Unmarshal(*must(index.SearchRaw(ctx, "rayuela", &meilisearch.SearchRequest{AttributesToRetrieve: []string{"id"}})), &r))
		eq(r.Hits, []doc{{"id": 3}})
	})
	checkMissing("Search: distinct", "distinctAttribute", func() {
		eq(ids(search("temporal", &meilisearch.SearchRequest{Distinct: "year"})), []int{6})
	})
	check("MultiSearch", func() {
		r := must(client.MultiSearch(ctx, &meilisearch.MultiSearchRequest{Queries: []*meilisearch.SearchRequest{{IndexUID: uid, Query: "neruda"}, {IndexUID: uid, Query: "bolaño"}}}))
		eq(len(r.Results), 2)
		eq(r.Results[0].IndexUID, uid)
		eq(ids(&r.Results[1])[0], 5)
	})
	checkMissing("MultiSearch (federated)", "federated search", func() {
		r := must(client.MultiSearch(ctx, &meilisearch.MultiSearchRequest{Federation: &meilisearch.MultiSearchFederation{}, Queries: []*meilisearch.SearchRequest{{IndexUID: uid, Query: "neruda"}}}))
		eq(len(r.Hits), 1)
	})
	check("GetDocument", func() {
		var d doc
		ok(index.GetDocument(ctx, "3", nil, &d))
		eq(d["title"], "Rayuela")
		var only doc
		ok(index.GetDocument(ctx, "3", &meilisearch.DocumentQuery{Fields: []string{"id"}}, &only))
		eq(only, doc{"id": 3})
	})
	check("GetDocuments (query + filter)", func() {
		var r meilisearch.DocumentsResult
		ok(index.GetDocuments(ctx, &meilisearch.DocumentsQuery{Limit: 3, Offset: 1, Fields: []string{"id"}}, &r))
		eq([]int64{int64(len(r.Results)), r.Total, r.Offset, r.Limit}, []int{3, 7, 1, 3})
		var first []doc
		ok(r.Results.DecodeInto(&first))
		eq(first[0], doc{"id": 2})
		var f meilisearch.DocumentsResult
		ok(index.GetDocuments(ctx, &meilisearch.DocumentsQuery{Filter: `author = "Isabel Allende"`}, &f))
		var found []doc
		ok(f.Results.DecodeInto(&found))
		eq(found[0]["id"], 2)
	})
	check("UpdateDocuments (partial)", func() {
		done(index.UpdateDocuments(ctx, []doc{{"id": 2, "price": 11.25}}, nil))
		var d doc
		ok(index.GetDocument(ctx, "2", nil, &d))
		eq(d["price"], 11.25)
		eq(d["author"], "Isabel Allende")
	})
	check("DeleteDocument", func() {
		done(index.DeleteDocument(ctx, "6", nil))
		var d doc
		rejects("document_not_found", index.GetDocument(ctx, "6", nil, &d))
	})
	check("DeleteDocuments (ids)", func() { eq(done(index.DeleteDocuments(ctx, []string{"7"}, nil)).Details.DeletedDocuments, 1) })
	check("DeleteDocumentsByFilter", func() {
		task := done(index.DeleteDocumentsByFilter(ctx, "year < 1930", nil))
		eq(task.Details.DeletedDocuments, 1)
	})
	check("GetStats (index and global)", func() {
		s := must(index.GetStats(ctx, nil))
		eq(s.NumberOfDocuments, 4)
		eq(s.IsIndexing, false)
		eq(s.FieldDistribution["title"], 4)
		g := must(client.GetStats(ctx, nil))
		eq(g.Indexes[uid].NumberOfDocuments, 4)
		truthy(!g.LastUpdate.IsZero(), "no lastUpdate")
	})
	check("GetTasks / GetTask", func() {
		r := must(client.GetTasks(ctx, &meilisearch.TasksQuery{IndexUIDS: []string{uid}, Limit: 3}))
		eq(len(r.Results), 3)
		truthy(r.Results[0].UID > r.Results[1].UID, "tasks are not newest first")
		truthy(r.Next != 0 && r.From == r.Results[0].UID, "bad cursor: from %d next %d", r.From, r.Next)
		eq(must(client.GetTask(ctx, r.Results[0].UID)).UID, r.Results[0].UID)
		eq(must(index.GetTask(ctx, r.Results[0].UID)).UID, r.Results[0].UID)
		for _, t := range must(index.GetTasks(ctx, &meilisearch.TasksQuery{})).Results {
			eq(t.IndexUID, uid)
		}
		// Deleting the missing index at the start is recorded as a failed task, as in Meilisearch.
		failed := must(client.GetTasks(ctx, &meilisearch.TasksQuery{Statuses: []meilisearch.TaskStatus{meilisearch.TaskStatusFailed}, IndexUIDS: []string{uid}}))
		eq(failed.Total, len(failed.Results))
		mine := 0
		for _, t := range failed.Results {
			eq(t.Status, "failed")
			if t.UID >= firstTask {
				eq(t.Error.Code, "index_not_found")
				mine++
			}
		}
		truthy(mine >= 1, "the failed deletion is not listed")
		typed := must(client.GetTasks(ctx, &meilisearch.TasksQuery{Types: []meilisearch.TaskType{meilisearch.TaskTypeDocumentAdditionOrUpdate}, IndexUIDS: []string{uid}}))
		truthy(len(typed.Results) > 0, "no document tasks")
		for _, t := range typed.Results {
			eq(t.Type, "documentAdditionOrUpdate")
		}
	})
	check("GetTasks: uids, reverse, dates", func() {
		newest := must(client.GetTasks(ctx, &meilisearch.TasksQuery{IndexUIDS: []string{uid}, Limit: 3})).Results
		picked := must(client.GetTasks(ctx, &meilisearch.TasksQuery{UIDS: []int64{newest[0].UID, newest[2].UID}}))
		eq([]int64{picked.Results[0].UID, picked.Results[1].UID, picked.Total}, []int64{newest[0].UID, newest[2].UID, 2})
		// Only the index-level listing of this client sends `reverse`.
		oldest := must(index.GetTasks(ctx, &meilisearch.TasksQuery{Reverse: true, From: firstTask, Limit: 2}))
		eq(oldest.Results[0].UID, firstTask)
		truthy(oldest.Results[1].UID > firstTask && oldest.Next > oldest.Results[1].UID, "not oldest first")
		future := time.Date(2099, 1, 1, 0, 0, 0, 0, time.UTC)
		eq(must(client.GetTasks(ctx, &meilisearch.TasksQuery{IndexUIDS: []string{uid}, AfterEnqueuedAt: future})).Total, 0)
		// A local time is sent with its offset.
		past := time.Date(2020, 1, 1, 0, 0, 0, 0, time.FixedZone("", 2*3600))
		truthy(must(client.GetTasks(ctx, &meilisearch.TasksQuery{IndexUIDS: []string{uid}, BeforeEnqueuedAt: future, AfterFinishedAt: past, Limit: 1})).Total >= 3, "date filters dropped tasks")
		eq(must(client.GetTasks(ctx, &meilisearch.TasksQuery{IndexUIDS: []string{uid}, CanceledBy: []int64{newest[0].UID}})).Total, 0)
	})
	check("failed task carries an error", func() {
		task := wait(client.CreateIndex(ctx, &meilisearch.IndexConfig{Uid: uid}))
		eq(task.Status, "failed")
		eq(task.Error.Code, "index_already_exists")
	})
	checkMissing("GetBatches", "batches", func() { must(client.GetBatches(ctx, nil)) })
	checkMissing("CancelTasks", "task cancellation", func() {
		must(client.CancelTasks(ctx, &meilisearch.CancelTasksQuery{Statuses: []meilisearch.TaskStatus{meilisearch.TaskStatusEnqueued}}))
	})
	check("keys: create, get, update, restricted use, delete", func() {
		k := must(client.CreateKey(ctx, &meilisearch.Key{Name: "solo libros", Actions: []string{"search"}, Indexes: []string{uid}}))
		truthy(k.Key != "" && k.UID != "", "no key or uid")
		eq(k.Actions, []string{"search"})
		eq(k.Indexes, []string{uid})
		truthy(k.ExpiresAt.IsZero() && !k.CreatedAt.IsZero(), "bad key times")
		eq(must(client.GetKey(ctx, k.UID)).Name, "solo libros")
		eq(must(client.GetKey(ctx, k.Key)).UID, k.UID)
		eq(must(client.UpdateKey(ctx, k.UID, &meilisearch.Key{Name: "solo libros", Description: "x"})).Description, "x")
		found := false
		for _, x := range must(client.GetKeys(ctx, &meilisearch.KeysQuery{Limit: 50})).Results {
			found = found || x.UID == k.UID
		}
		truthy(found, "key not listed")
		limited := meilisearch.New(host, meilisearch.WithAPIKey(k.Key))
		defer limited.Close()
		r := must(limited.Index(uid).Search(ctx, "rayuela", &meilisearch.SearchRequest{}))
		eq(ids(r)[0], 3)
		_, err := limited.Index("products").Search(ctx, "x", &meilisearch.SearchRequest{})
		rejects("invalid_api_key", err)
		_, err = limited.Index(uid).AddDocuments(ctx, []doc{{"id": 99}}, nil)
		rejects("invalid_api_key", err)
		truthy(must(client.DeleteKey(ctx, k.UID)), "key not deleted")
		_, err = limited.Index(uid).Search(ctx, "rayuela", &meilisearch.SearchRequest{})
		rejects("invalid_api_key", err)
	})
	check("keys: expiry", func() {
		expires := time.Date(2099, 1, 1, 0, 0, 0, 0, time.UTC)
		k := must(client.CreateKey(ctx, &meilisearch.Key{Actions: []string{"*"}, Indexes: []string{"*"}, ExpiresAt: expires}))
		truthy(k.ExpiresAt.Equal(expires), "expiresAt is %s", k.ExpiresAt)
		truthy(must(client.GetKey(ctx, k.UID)).ExpiresAt.Equal(expires), "expiresAt changed")
		must(client.DeleteKey(ctx, k.UID))
	})
	check("FetchPrimaryKey / UpdateIndex", func() {
		eq(*must(index.FetchPrimaryKey(ctx)), "id")
		tmp := uid + "_tmp"
		wait(client.DeleteIndex(ctx, tmp))
		done(client.CreateIndex(ctx, &meilisearch.IndexConfig{Uid: tmp}))
		eq(must(client.GetIndex(ctx, tmp)).PrimaryKey, "")
		done(client.Index(tmp).UpdateIndex(ctx, &meilisearch.UpdateIndexRequestParams{PrimaryKey: "isbn"}))
		eq(must(client.GetIndex(ctx, tmp)).PrimaryKey, "isbn")
		done(client.DeleteIndex(ctx, tmp))
	})
	checkMissing("SwapIndexes", "index swaps", func() {
		done(client.SwapIndexes(ctx, []*meilisearch.SwapIndexesParams{{Indexes: []string{uid, uid + "_other"}}}))
	})
	check("DeleteAllDocuments", func() {
		done(index.DeleteAllDocuments(ctx, nil))
		eq(must(index.GetStats(ctx, nil)).NumberOfDocuments, 0)
	})
	check("ResetSettings", func() {
		done(index.ResetSettings(ctx))
		eq(len(*must(index.GetFilterableAttributes(ctx))), 0)
	})
	check("DeleteIndex", func() {
		done(client.DeleteIndex(ctx, uid))
		_, err := client.GetIndex(ctx, uid)
		rejects("index_not_found", err)
	})

	fmt.Printf("\n%d passed, %d failed, %d expected failures (missing features)\n", passed, len(failures), len(expected))
	if len(failures) > 0 {
		fmt.Println("failed: " + strings.Join(failures, " | "))
		os.Exit(1)
	}
}
