<?php

// Drives a running server through the official meilisearch-php client.
//
// Mirrors tests/sdk/compat.mjs. Start the server with
//     trailhound --data-dir ./data-test --addr 127.0.0.1:7711 --master-key test-master-key
// and run `composer install && php compat.php`.

declare(strict_types=1);

require __DIR__.'/vendor/autoload.php';

use Meilisearch\Client;
use Meilisearch\Contracts\DocumentsQuery;
use Meilisearch\Contracts\IndexesQuery;
use Meilisearch\Contracts\KeysQuery;
use Meilisearch\Contracts\MultiSearchFederation;
use Meilisearch\Contracts\SearchQuery;
use Meilisearch\Contracts\TasksQuery;
use Meilisearch\Exceptions\ApiException;

$host = getenv('TRAILHOUND_URL') ?: 'http://127.0.0.1:7711';
$key = getenv('TRAILHOUND_KEY') ?: 'test-master-key';
const UID = 'php_books';

$client = new Client($host, $key);
$index = $client->index(UID);
$passed = 0;
$failures = [];
$expected = [];
// Tasks of earlier runs against the same server are ignored.
$firstTask = 0;

/**
 * Run one check. `$missing` names the documented missing feature that makes
 * the check fail; such a check must fail, so the list stays honest.
 */
function check(string $name, callable $fn, ?string $missing = null): void
{
    global $passed, $failures, $expected;
    try {
        $fn();
    } catch (Throwable $e) {
        $line = substr(strtok($e->getMessage(), "\n") ?: get_class($e), 0, 220);
        if (null !== $missing) {
            $expected[] = $name;
            echo "xfail {$name} ({$missing}) -> {$line}\n";
        } else {
            $failures[] = $name;
            echo "FAIL  {$name} -> ".get_class($e).": {$line} (line {$e->getLine()})\n";
        }

        return;
    }
    if (null !== $missing) {
        $failures[] = $name;
        echo "FAIL  {$name} -> passed, but is listed as missing: {$missing}\n";
    } else {
        ++$passed;
        echo "ok    {$name}\n";
    }
}

function truthy(bool $cond, string $message = 'assertion failed'): void
{
    if (!$cond) {
        throw new RuntimeException($message);
    }
}

function eq(mixed $got, mixed $want): void
{
    if ($got !== $want) {
        throw new RuntimeException('got '.json_encode($got, JSON_UNESCAPED_UNICODE).', want '.json_encode($want, JSON_UNESCAPED_UNICODE));
    }
}

function done(array $info): array
{
    global $client;
    $task = $client->waitForTask($info['taskUid']);
    truthy('succeeded' === $task['status'], 'task '.$task['uid'].': '.json_encode($task['error']));

    return $task;
}

function rejects(string $code, callable $fn): void
{
    try {
        $fn();
    } catch (ApiException $e) {
        eq($e->errorCode, $code);

        return;
    }
    throw new RuntimeException("expected the error {$code}");
}

function ids(iterable $hits): array
{
    $out = [];
    foreach ($hits as $hit) {
        $out[] = $hit['id'];
    }

    return $out;
}

$books = [
    ['id' => 1, 'title' => 'Cien años de soledad', 'author' => 'Gabriel García Márquez', 'year' => 1967, 'genres' => ['novela', 'realismo mágico'], 'price' => 12.5],
    ['id' => 2, 'title' => 'La casa de los espíritus', 'author' => 'Isabel Allende', 'year' => 1982, 'genres' => ['novela'], 'price' => 10],
    ['id' => 3, 'title' => 'Rayuela', 'author' => 'Julio Cortázar', 'year' => 1963, 'genres' => ['novela', 'experimental'], 'price' => 14],
    ['id' => 4, 'title' => 'Veinte poemas de amor y una canción desesperada', 'author' => 'Pablo Neruda', 'year' => 1924, 'genres' => ['poesía'], 'price' => 8],
    ['id' => 5, 'title' => 'Los detectives salvajes', 'author' => 'Roberto Bolaño', 'year' => 1998, 'genres' => ['novela'], 'price' => 16],
];

check('health / isHealthy', function () use ($client) {
    eq($client->health()['status'], 'available');
    eq($client->isHealthy(), true);
});
check('version', function () use ($client) { truthy('' !== $client->version()['pkgVersion']); });
check('deleteIndex (missing index)', function () use ($client, &$firstTask) {
    $info = $client->deleteIndex(UID);
    $firstTask = $info['taskUid'];
    $task = $client->waitForTask($info['taskUid']);
    if ('succeeded' === $task['status']) { // left over from an interrupted run
        $task = $client->waitForTask($client->deleteIndex(UID)['taskUid']);
    }
    eq($task['status'], 'failed');
    eq($task['error']['code'], 'index_not_found');
});
check('createIndex + waitForTask', function () use ($client) {
    $task = done($client->createIndex(UID, ['primaryKey' => 'id']));
    eq($task['type'], 'indexCreation');
    eq($task['indexUid'], UID);
    truthy(isset($task['enqueuedAt'], $task['startedAt'], $task['finishedAt'], $task['duration']));
});
check('getIndex / getRawIndex / fetchInfo', function () use ($client, $index) {
    $ix = $client->getIndex(UID);
    eq($ix->getPrimaryKey(), 'id');
    truthy($ix->getCreatedAt() instanceof DateTimeInterface && $ix->getUpdatedAt() instanceof DateTimeInterface);
    eq($client->getRawIndex(UID)['uid'], UID);
    eq($index->fetchInfo()->getUid(), UID);
    eq($index->fetchRawInfo()['primaryKey'], 'id');
});
check('getIndexes', function () use ($client) {
    $r = $client->getIndexes((new IndexesQuery())->setLimit(50));
    truthy(in_array(UID, array_map(fn ($ix) => $ix->getUid(), $r->getResults()), true));
    truthy($r->getTotal() >= 1 && 50 === $r->getLimit() && 0 === $r->getOffset());
});
check('addDocuments', function () use ($index, $books) {
    $task = done($index->addDocuments($books));
    eq($task['details']['indexedDocuments'], 5);
    eq($task['details']['receivedDocuments'], 5);
});
check('addDocumentsInBatches', function () use ($index) {
    // Documents 6 and 7 share a year, for the distinct check.
    $tasks = $index->addDocumentsInBatches([['id' => 6, 'title' => 'Temporal', 'year' => 2000], ['id' => 7, 'title' => 'Temporal dos', 'year' => 2000]], 1);
    eq(count($tasks), 2);
    array_map('done', $tasks);
});
check('addDocumentsNdjson / addDocumentsJson', function () use ($index) {
    done($index->addDocumentsNdjson("{\"id\": 8, \"title\": \"Linea uno\"}\n{\"id\": 9, \"title\": \"Linea dos\"}\n"));
    done($index->addDocumentsJson('[{"id": 10, "title": "Texto json"}]'));
    eq($index->getDocument(9)['title'], 'Linea dos');
    eq($index->getDocument(10)['title'], 'Texto json');
    eq(done($index->deleteDocuments([8, 9, 10]))['details']['deletedDocuments'], 3);
});
check('addDocumentsCsv', function () use ($index) {
    done($index->addDocumentsCsv("id,title\n11,Desde csv\n"));
}, 'CSV documents');
check('updateSettings', function () use ($index) {
    done($index->updateSettings(['filterableAttributes' => ['genres', 'year', 'author'], 'sortableAttributes' => ['year', 'price'], 'stopWords' => ['de', 'la', 'los']]));
});
check('getSettings', function () use ($index) {
    $s = $index->getSettings();
    eq($s['sortableAttributes'], ['year', 'price']);
    truthy(is_array($s['rankingRules']) && count($s['rankingRules']) > 0);
    eq($s['pagination']['maxTotalHits'], 1000);
});
check('get/update/reset single settings', function () use ($index) {
    done($index->updateSynonyms(['libro' => ['novela']]));
    eq($index->getSynonyms(), ['libro' => ['novela']]);
    done($index->updateTypoTolerance(['minWordSizeForTypos' => ['oneTypo' => 4]]));
    eq($index->getTypoTolerance()['minWordSizeForTypos']['oneTypo'], 4);
    done($index->resetTypoTolerance());
    eq($index->getTypoTolerance()['minWordSizeForTypos']['oneTypo'], 5);
    $filterable = $index->getFilterableAttributes();
    sort($filterable);
    eq($filterable, ['author', 'genres', 'year']);
    eq($index->getSortableAttributes(), ['year', 'price']);
    eq($index->getSearchableAttributes(), ['*']);
    eq($index->getDisplayedAttributes(), ['*']);
    eq($index->getPagination()['maxTotalHits'], 1000);
    eq($index->getFaceting()['maxValuesPerFacet'], 100);
    eq($index->getDistinctAttribute(), null);
    $stopWords = $index->getStopWords();
    sort($stopWords);
    eq($stopWords, ['de', 'la', 'los']);
    $rules = ['words', 'typo', 'proximity', 'attribute', 'sort', 'exactness'];
    done($index->updateRankingRules($rules));
    eq($index->getRankingRules(), $rules);
    done($index->updateSearchableAttributes(['title', 'author']));
    eq($index->getSearchableAttributes(), ['title', 'author']);
    done($index->resetSearchableAttributes());
    done($index->updateDisplayedAttributes(['id', 'title', 'author', 'year', 'genres', 'price']));
    done($index->resetDisplayedAttributes());
    done($index->updatePagination(['maxTotalHits' => 500]));
    eq($index->getPagination()['maxTotalHits'], 500);
    done($index->resetPagination());
    done($index->updateFaceting(['maxValuesPerFacet' => 50]));
    eq($index->getFaceting()['maxValuesPerFacet'], 50);
    done($index->resetFaceting());
});
check('search: typo + accents', function () use ($index) {
    $r = $index->search('garsia marques');
    eq($r->getHit(0)['id'], 1);
    eq($r->getQuery(), 'garsia marques');
    eq($r->getEstimatedTotalHits(), 1);
    eq($r->getHitsCount(), 1);
    truthy($r->getProcessingTimeMs() >= 0);
});
check('search: filter + sort + facets', function () use ($index) {
    $r = $index->search('', ['filter' => ['genres = novela', 'year > 1965'], 'sort' => ['year:desc'], 'facets' => ['genres']]);
    eq(ids($r->getHits()), [5, 2, 1]);
    eq($r->getFacetDistribution()['genres']['novela'], 3);
    $r = $index->search('', ['filter' => 'author = "Isabel Allende" OR year IN [1963, 1998]', 'sort' => ['price:asc']]);
    eq(ids($r->getHits()), [2, 3, 5]);
});
check('search: phrase', function () use ($index) {
    eq(ids($index->search('"poemas de amor"')->getHits()), [4]);
    eq($index->search('"amor poemas"')->getHitsCount(), 0);
});
check('search: highlight, crop, retrieve, matches', function () use ($index) {
    $r = $index->search('soledad', ['attributesToRetrieve' => ['title'], 'attributesToHighlight' => ['title'], 'highlightPreTag' => '<b>', 'highlightPostTag' => '</b>', 'showMatchesPosition' => true]);
    $hit = $r->getHit(0);
    $keys = array_keys($hit);
    sort($keys);
    eq($keys, ['_formatted', '_matchesPosition', 'title']);
    eq($hit['_formatted']['title'], 'Cien años de <b>soledad</b>');
    eq(count($hit['_matchesPosition']['title']), 1);
});
check('search: offset / limit', function () use ($index) {
    $r = $index->search('', ['offset' => 1, 'limit' => 2, 'sort' => ['year:asc']]);
    eq(ids($r->getHits()), [3, 1]);
    eq([$r->getOffset(), $r->getLimit()], [1, 2]);
});
check('search: page / hitsPerPage', function () use ($index) {
    $r = $index->search('', ['page' => 2, 'hitsPerPage' => 2]);
    eq($r->getHitsCount(), 2);
    eq([$r->getTotalHits(), $r->getTotalPages(), $r->getPage(), $r->getHitsPerPage()], [7, 4, 2, 2]);
});
check('search: synonym', function () use ($index) { truthy($index->search('libro', ['limit' => 20])->getHitsCount() >= 4); });
check('search: bad filter gives a typed error', function () use ($index) {
    rejects('invalid_search_filter', fn () => $index->search('', ['filter' => 'title = x']));
});
check('rawSearch', function () use ($index) {
    $r = $index->rawSearch('rayuela', ['attributesToRetrieve' => ['id']]);
    eq($r['hits'], [['id' => 3]]);
    eq($r['estimatedTotalHits'], 1);
});
check('search: distinct', function () use ($index) {
    eq(ids($index->search('temporal', ['distinct' => 'year'])->getHits()), [6]);
}, 'distinctAttribute');
check('multiSearch', function () use ($client) {
    $r = $client->multiSearch([(new SearchQuery())->setIndexUid(UID)->setQuery('neruda'), (new SearchQuery())->setIndexUid(UID)->setQuery('bolaño')]);
    eq(count($r['results']), 2);
    eq($r['results'][0]['indexUid'], UID);
    eq($r['results'][1]['hits'][0]['id'], 5);
});
check('multiSearch (federated)', function () use ($client) {
    $r = $client->multiSearch([(new SearchQuery())->setIndexUid(UID)->setQuery('neruda')], new MultiSearchFederation());
    eq(count($r['hits']), 1);
}, 'federated search');
check('getDocument', function () use ($index) {
    eq($index->getDocument(3)['title'], 'Rayuela');
    eq($index->getDocument(3, ['id']), ['id' => 3]);
});
check('getDocuments (query + filter)', function () use ($index) {
    $r = $index->getDocuments((new DocumentsQuery())->setLimit(3)->setOffset(1)->setFields(['id']));
    eq([count($r->getResults()), $r->getTotal(), $r->getOffset(), $r->getLimit()], [3, 7, 1, 3]);
    eq($r->getResults()[0], ['id' => 2]);
    $f = $index->getDocuments((new DocumentsQuery())->setFilter(['author = "Isabel Allende"']));
    eq($f->getResults()[0]['id'], 2);
});
check('updateDocuments (partial)', function () use ($index) {
    done($index->updateDocuments([['id' => 2, 'price' => 11.25]]));
    $d = $index->getDocument(2);
    eq($d['price'], 11.25);
    eq($d['author'], 'Isabel Allende');
});
check('deleteDocument', function () use ($index) {
    done($index->deleteDocument(6));
    rejects('document_not_found', fn () => $index->getDocument(6));
});
check('deleteDocuments (ids)', function () use ($index) { eq(done($index->deleteDocuments([7]))['details']['deletedDocuments'], 1); });
check('deleteDocuments (filter)', function () use ($index) {
    eq(done($index->deleteDocuments(['filter' => 'year < 1930']))['details']['deletedDocuments'], 1);
});
check('stats (index and global)', function () use ($client, $index) {
    $s = $index->stats();
    eq($s['numberOfDocuments'], 4);
    eq($s['isIndexing'], false);
    eq($s['fieldDistribution']['title'], 4);
    eq($client->stats()['indexes'][UID]['numberOfDocuments'], 4);
});
check('getTasks / getTask', function () use ($client, $index, &$firstTask) {
    $r = $client->getTasks((new TasksQuery())->setIndexUids([UID])->setLimit(3));
    eq(count($r->getResults()), 3);
    $newest = $r->getResults();
    truthy($newest[0]['uid'] > $newest[1]['uid']);
    truthy($r->getNext() > 0 && $r->getFrom() === $newest[0]['uid']);
    eq($client->getTask($newest[0]['uid'])['uid'], $newest[0]['uid']);
    eq($index->getTask($newest[0]['uid'])['uid'], $newest[0]['uid']);
    foreach ($index->getTasks()->getResults() as $t) {
        eq($t['indexUid'], UID);
    }
    // Deleting the missing index at the start is recorded as a failed task, as in Meilisearch.
    $failed = $client->getTasks((new TasksQuery())->setStatuses(['failed'])->setIndexUids([UID]));
    eq($failed->getTotal(), count($failed->getResults()));
    $mine = 0;
    foreach ($failed->getResults() as $t) {
        eq($t['status'], 'failed');
        if ($t['uid'] >= $firstTask) {
            eq($t['error']['code'], 'index_not_found');
            ++$mine;
        }
    }
    truthy($mine >= 1);
    $typed = $client->getTasks((new TasksQuery())->setTypes(['documentAdditionOrUpdate'])->setIndexUids([UID]));
    truthy(count($typed->getResults()) > 0);
    foreach ($typed->getResults() as $t) {
        eq($t['type'], 'documentAdditionOrUpdate');
    }
});
check('getTasks: uids, reverse, dates', function () use ($client, &$firstTask) {
    $newest = $client->getTasks((new TasksQuery())->setIndexUids([UID])->setLimit(3))->getResults();
    $picked = $client->getTasks((new TasksQuery())->setUids([$newest[0]['uid'], $newest[2]['uid']]));
    eq(array_column($picked->getResults(), 'uid'), [$newest[0]['uid'], $newest[2]['uid']]);
    eq($picked->getTotal(), 2);
    $oldest = $client->getTasks((new TasksQuery())->setIndexUids([UID])->setReverse(true)->setFrom($firstTask)->setLimit(2));
    eq($oldest->getResults()[0]['uid'], $firstTask);
    truthy($oldest->getResults()[1]['uid'] > $firstTask && $oldest->getNext() > $oldest->getResults()[1]['uid']);
    $future = new DateTimeImmutable('2099-01-01T00:00:00Z');
    eq($client->getTasks((new TasksQuery())->setIndexUids([UID])->setAfterEnqueuedAt($future))->getTotal(), 0);
    // A local time is sent with its offset.
    $past = new DateTimeImmutable('2020-01-01T00:00:00+02:00');
    truthy($client->getTasks((new TasksQuery())->setIndexUids([UID])->setBeforeEnqueuedAt($future)->setAfterFinishedAt($past)->setLimit(1))->getTotal() >= 3);
    eq($client->getTasks((new TasksQuery())->setIndexUids([UID])->setCanceledBy([$newest[0]['uid']]))->getTotal(), 0);
});
check('waitForTasks', function () use ($client, $index) {
    $a = $index->updateDocuments([['id' => 1, 'price' => 12.5]]);
    $b = $index->updateDocuments([['id' => 3, 'price' => 14]]);
    $tasks = $client->waitForTasks([$a['taskUid'], $b['taskUid']]);
    eq(array_column($tasks, 'status'), ['succeeded', 'succeeded']);
});
check('failed task carries an error', function () use ($client) {
    $task = $client->waitForTask($client->createIndex(UID)['taskUid']);
    eq($task['status'], 'failed');
    eq($task['error']['code'], 'index_already_exists');
});
check('getBatches', function () use ($client) { $client->getBatches(); }, 'batches');
check('cancelTasks', function () use ($client) {
    $client->cancelTasks((new Meilisearch\Contracts\CancelTasksQuery())->setStatuses(['enqueued']));
}, 'task cancellation');
check('keys: create, get, update, restricted use, delete', function () use ($client, $host) {
    $k = $client->createKey(['name' => 'solo libros', 'actions' => ['search'], 'indexes' => [UID], 'expiresAt' => null]);
    truthy('' !== (string) $k->getKey() && '' !== (string) $k->getUid());
    eq([$k->getActions(), $k->getIndexes(), $k->getExpiresAt()], [['search'], [UID], null]);
    truthy($k->getCreatedAt() instanceof DateTimeInterface);
    eq($client->getKey($k->getUid())->getName(), 'solo libros');
    eq($client->getKey($k->getKey())->getUid(), $k->getUid());
    eq($client->updateKey($k->getUid(), ['description' => 'x'])->getDescription(), 'x');
    $listed = array_map(fn ($x) => $x->getUid(), $client->getKeys((new KeysQuery())->setLimit(50))->getResults());
    truthy(in_array($k->getUid(), $listed, true));
    $limited = new Client($host, $k->getKey());
    eq($limited->index(UID)->search('rayuela')->getHit(0)['id'], 3);
    rejects('invalid_api_key', fn () => $limited->index('products')->search('x'));
    rejects('invalid_api_key', fn () => $limited->index(UID)->addDocuments([['id' => 99]]));
    $client->deleteKey($k->getUid());
    rejects('invalid_api_key', fn () => $limited->index(UID)->search('rayuela'));
});
check('keys: expiry', function () use ($client) {
    $k = $client->createKey(['actions' => ['*'], 'indexes' => ['*'], 'expiresAt' => '2099-01-01T00:00:00Z']);
    eq($k->getExpiresAt()->format('Y-m-d H:i:s'), '2099-01-01 00:00:00');
    eq($client->getKey($k->getUid())->getExpiresAt()->format('Y'), '2099');
    $client->deleteKey($k->getUid());
});
check('fetchPrimaryKey / updateIndex', function () use ($client, $index) {
    eq($index->fetchPrimaryKey(), 'id');
    $tmp = UID.'_tmp';
    $client->waitForTask($client->deleteIndex($tmp)['taskUid']);
    done($client->createIndex($tmp));
    eq($client->index($tmp)->fetchPrimaryKey(), null);
    done($client->updateIndex($tmp, ['primaryKey' => 'isbn']));
    eq($client->index($tmp)->fetchPrimaryKey(), 'isbn');
    done($client->index($tmp)->delete());
});
check('swapIndexes', function () use ($client) {
    done($client->swapIndexes([[UID, UID.'_other']]));
}, 'index swaps');
check('deleteAllDocuments', function () use ($index) {
    done($index->deleteAllDocuments());
    eq($index->stats()['numberOfDocuments'], 0);
});
check('resetSettings', function () use ($index) {
    done($index->resetSettings());
    eq($index->getFilterableAttributes(), []);
});
check('deleteIndex', function () use ($client) {
    done($client->deleteIndex(UID));
    rejects('index_not_found', fn () => $client->getIndex(UID));
});

echo "\n{$passed} passed, ".count($failures).' failed, '.count($expected)." expected failures (missing features)\n";
if ($failures) {
    echo 'failed: '.implode(' | ', $failures)."\n";
    exit(1);
}
