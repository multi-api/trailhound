<?php

// Drives a running server through Laravel Scout's Meilisearch driver, from a
// minimal Laravel application (this folder).
//
// Start the server with
//     trailhound --data-dir ./data-test --addr 127.0.0.1:7711 --master-key test-master-key
// and run `composer install && cp .env.example .env && php compat.php`.

declare(strict_types=1);

use App\Models\Book;
use Illuminate\Contracts\Console\Kernel;
use Illuminate\Pagination\LengthAwarePaginator;
use Illuminate\Support\Facades\Artisan;
use Meilisearch\Client;
use Meilisearch\Contracts\TasksQuery;

require __DIR__.'/vendor/autoload.php';

$app = require __DIR__.'/bootstrap/app.php';
$app->make(Kernel::class)->bootstrap();

const INDEX = 'laravel_books'; // SCOUT_PREFIX + the model's table

$passed = 0;
$failures = [];

function check(string $name, callable $fn): void
{
    global $passed, $failures;
    try {
        $fn();
    } catch (Throwable $e) {
        $failures[] = $name;
        $line = substr(strtok($e->getMessage(), "\n") ?: get_class($e), 0, 220);
        echo "FAIL  {$name} -> ".get_class($e).": {$line} (line {$e->getLine()})\n";

        return;
    }
    ++$passed;
    echo "ok    {$name}\n";
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

function client(): Client
{
    return app(Client::class);
}

/**
 * Scout does not wait for the server: writes are tasks. Wait until none is
 * left, and return the ones this index failed since `$since`.
 */
function settle(int $since = 0): array
{
    do {
        $pending = client()->getTasks((new TasksQuery())->setStatuses(['enqueued', 'processing'])->setLimit(100))->getResults();
        if ($pending) {
            client()->waitForTasks(array_column($pending, 'uid'));
        }
    } while ($pending);
    $failed = client()->getTasks((new TasksQuery())->setStatuses(['failed'])->setIndexUids([INDEX])->setLimit(100))->getResults();

    return array_values(array_filter($failed, fn ($t) => $t['uid'] > $since));
}

/** Run a real `php artisan` process, as a developer would. */
function artisan(string ...$arguments): string
{
    $command = array_merge([PHP_BINARY, __DIR__.'/artisan'], $arguments, ['--no-interaction', '--no-ansi']);
    $process = proc_open($command, [1 => ['pipe', 'w'], 2 => ['pipe', 'w']], $pipes, __DIR__);
    $output = stream_get_contents($pipes[1]).stream_get_contents($pipes[2]);
    $status = proc_close($process);
    truthy(0 === $status, 'artisan '.implode(' ', $arguments)." exited with {$status}: {$output}");

    return $output;
}

function ids(iterable $models): array
{
    $out = [];
    foreach ($models as $model) {
        $out[] = $model->id;
    }

    return $out;
}

function documents(): int
{
    return client()->index(INDEX)->stats()['numberOfDocuments'];
}

$books = [
    ['id' => 1, 'title' => 'Cien años de soledad', 'author' => 'Gabriel García Márquez', 'year' => 1967, 'genres' => ['novela', 'realismo mágico'], 'price' => 12.5, 'in_print' => true, 'translator' => null],
    ['id' => 2, 'title' => 'La casa de los espíritus', 'author' => 'Isabel Allende', 'year' => 1982, 'genres' => ['novela'], 'price' => 10, 'in_print' => true, 'translator' => null],
    ['id' => 3, 'title' => 'Rayuela', 'author' => 'Julio Cortázar', 'year' => 1963, 'genres' => ['novela', 'experimental'], 'price' => 14, 'in_print' => false, 'translator' => null],
    ['id' => 4, 'title' => 'Veinte poemas de amor y una canción desesperada', 'author' => 'Pablo Neruda', 'year' => 1924, 'genres' => ['poesía'], 'price' => 8, 'in_print' => true, 'translator' => null],
    ['id' => 5, 'title' => 'Los detectives salvajes', 'author' => 'Roberto Bolaño', 'year' => 1998, 'genres' => ['novela'], 'price' => 16, 'in_print' => true, 'translator' => null],
    ['id' => 6, 'title' => 'El extranjero', 'author' => 'Albert Camus', 'year' => 1942, 'genres' => ['novela'], 'price' => 9, 'in_print' => true, 'translator' => 'Bonifacio del Carril'],
];

// A clean database and a clean index.
if (!file_exists(config('database.connections.sqlite.database'))) {
    touch(config('database.connections.sqlite.database'));
}
Artisan::call('migrate:fresh', ['--force' => true]);
client()->deleteIndex(INDEX);
settle();
$start = client()->getTasks((new TasksQuery())->setLimit(1))->getResults()[0]['uid'];

check('scout:index creates the index', function () {
    artisan('scout:index', 'books', '--key=id');
    settle();
    eq(client()->getIndex(INDEX)->getPrimaryKey(), 'id');
});
check('scout:sync-index-settings', function () use ($start) {
    $output = artisan('scout:sync-index-settings');
    truthy(str_contains($output, 'synced successfully'), $output);
    eq(settle($start), []);
    $settings = client()->index(INDEX)->getSettings();
    eq($settings['sortableAttributes'], ['year', 'price']);
    $filterable = $settings['filterableAttributes'];
    sort($filterable);
    eq($filterable, ['author', 'genres', 'in_print', 'price', 'translator', 'year']);
});
check('saving a model indexes it', function () use ($books, $start) {
    foreach ($books as $book) {
        Book::create($book);
    }
    eq(settle($start), []);
    eq(documents(), 6);
    $document = client()->index(INDEX)->getDocument(1);
    eq($document['title'], 'Cien años de soledad');
    eq($document['genres'], ['novela', 'realismo mágico']);
    eq($document['in_print'], true);
});
check('Model::search (typo + accents)', function () {
    $found = Book::search('garsia marques')->get();
    eq(ids($found), [1]);
    truthy($found->first() instanceof Book);
    eq($found->first()->author, 'Gabriel García Márquez');
    eq(Book::search('rayuela')->first()->id, 3);
    eq(Book::search('no such book anywhere')->get()->count(), 0);
});
check('where', function () {
    eq(ids(Book::search('')->where('year', 1967)->get()), [1]);
    eq(ids(Book::search('')->where('author', 'Isabel Allende')->get()), [2]);
    eq(ids(Book::search('casa')->where('author', 'Isabel Allende')->where('year', 1982)->get()), [2]);
    eq(ids(Book::search('')->where('in_print', false)->get()), [3]);
    eq(Book::search('')->where('author', 'Nobody "Quoted" Here')->get()->count(), 0);
});
check('where with an operator', function () {
    eq(ids(Book::search('')->where('year', '>=', 1982)->orderBy('year')->get()), [2, 5]);
    eq(ids(Book::search('')->where('year', '<', 1960)->orderBy('year')->get()), [4, 6]);
    eq(ids(Book::search('')->where('price', '>', 12.5)->orderBy('price')->get()), [3, 5]);
    eq(ids(Book::search('')->where('year', '!=', 1967)->where('year', '>', 1963)->orderBy('year')->get()), [2, 5]);
});
check('where null / not null', function () {
    eq(ids(Book::search('')->where('translator', '!=', null)->get()), [6]);
    eq(ids(Book::search('')->where('translator', null)->orderBy('year')->get()), [4, 3, 1, 2, 5]);
});
check('whereIn / whereNotIn', function () {
    eq(ids(Book::search('')->whereIn('genres', ['poesía', 'experimental'])->orderBy('year')->get()), [4, 3]);
    eq(ids(Book::search('')->whereIn('year', [1967, 1998])->orderBy('year')->get()), [1, 5]);
    eq(ids(Book::search('')->whereNotIn('year', [1967, 1982, 1942])->orderBy('year')->get()), [4, 3, 5]);
    eq(ids(Book::search('')->whereIn('year', [1982, 1963])->where('in_print', true)->get()), [2]);
});
check('orderBy / take', function () {
    eq(ids(Book::search('')->orderBy('year', 'desc')->get()), [5, 2, 1, 3, 6, 4]);
    eq(ids(Book::search('')->orderBy('price')->take(2)->get()), [4, 6]);
    eq(ids(Book::search('')->where('genres', 'novela')->orderByDesc('price')->take(3)->get()), [5, 3, 1]);
});
check('paginate', function () {
    $page = Book::search('')->orderBy('year')->paginate(2, 'page', 2);
    truthy($page instanceof LengthAwarePaginator);
    eq(ids($page->items()), [3, 1]);
    eq([$page->total(), $page->lastPage(), $page->currentPage(), $page->perPage()], [6, 3, 2, 2]);
    $last = Book::search('')->where('genres', 'novela')->orderBy('year')->paginate(2, 'page', 3);
    eq(ids($last->items()), [5]);
    eq([$last->total(), $last->lastPage()], [5, 3]);
    eq(Book::search('salvajes')->paginate(10)->total(), 1);
});
check('simplePaginate / paginateRaw', function () {
    $page = Book::search('')->orderBy('year')->simplePaginate(4, 'page', 1);
    eq(ids($page->items()), [4, 6, 3, 1]);
    truthy($page->hasMorePages());
    $raw = Book::search('')->orderBy('year')->paginateRaw(4, 'page', 2);
    eq(array_column($raw->items()['hits'], 'id'), [2, 5]);
    eq($raw->total(), 6);
});
check('keys / raw / cursor / first', function () {
    eq(Book::search('')->orderBy('year')->keys()->all(), [4, 6, 3, 1, 2, 5]);
    $raw = Book::search('neruda')->raw();
    eq(array_column($raw['hits'], 'id'), [4]);
    truthy(isset($raw['processingTimeMs'], $raw['query']));
    eq(ids(Book::search('')->orderBy('year')->cursor()), [4, 6, 3, 1, 2, 5]);
    eq(Book::search('')->orderByDesc('price')->first()->id, 5);
});
check('options and search callback', function () {
    $raw = Book::search('soledad')->options(['attributesToHighlight' => ['title'], 'highlightPreTag' => '<b>', 'highlightPostTag' => '</b>'])->raw();
    eq($raw['hits'][0]['_formatted']['title'], 'Cien años de <b>soledad</b>');
    // `attributesToRetrieve` always gets the key prepended by Scout.
    $raw = Book::search('soledad')->options(['attributesToRetrieve' => ['title']])->raw();
    eq($raw['hits'][0], ['id' => 1, 'title' => 'Cien años de soledad']);
    $found = Book::search('soledad', function ($index, $query, $options) {
        $options['facets'] = ['genres'];

        return $index->search($query, $options);
    })->raw();
    eq($found['facetDistribution']['genres']['novela'], 1);
    // The highlighted version reaches the model as Scout metadata.
    $model = Book::search('soledad')->options(['attributesToHighlight' => ['title']])->first();
    eq($model->scoutMetadata()['_formatted']['title'], 'Cien años de <em>soledad</em>');
});
check('updating a model updates the index', function () use ($start) {
    Book::find(2)->update(['price' => 11.25, 'in_print' => false]);
    eq(settle($start), []);
    eq(ids(Book::search('')->where('in_print', false)->orderBy('year')->get()), [3, 2]);
    eq(client()->index(INDEX)->getDocument(2)['price'], 11.25);
    eq(documents(), 6);
});
check('deleting a model removes it', function () use ($start) {
    Book::find(6)->delete();
    eq(settle($start), []);
    eq(documents(), 5);
    eq(Book::search('extranjero')->get()->count(), 0);
});
check('unsearchable / searchable', function () use ($start) {
    Book::where('year', '<', 1950)->unsearchable();
    eq(settle($start), []);
    eq(Book::search('neruda')->get()->count(), 0);
    Book::where('year', '<', 1950)->searchable();
    eq(settle($start), []);
    eq(ids(Book::search('neruda')->get()), [4]);
});
check('scout:flush', function () use ($start) {
    $output = artisan('scout:flush', Book::class);
    truthy(str_contains($output, 'have been flushed'), $output);
    eq(settle($start), []);
    eq(documents(), 0);
    eq(Book::search('')->get()->count(), 0);
});
check('scout:import', function () use ($start) {
    $output = artisan('scout:import', Book::class);
    truthy(str_contains($output, 'have been imported'), $output);
    eq(settle($start), []);
    eq(documents(), 5);
    eq(ids(Book::search('')->orderBy('year')->get()), [4, 3, 1, 2, 5]);
});
check('scout:import --fresh, in chunks', function () use ($start) {
    artisan('scout:import', Book::class, '--fresh', '--chunk=2');
    eq(settle($start), []);
    eq(documents(), 5);
    eq(ids(Book::search('bolaño')->get()), [5]);
});
check('results follow the database', function () {
    // A row deleted behind Scout's back is still indexed, and is left out.
    Book::withoutSyncingToSearch(fn () => Book::find(5)->delete());
    eq(documents(), 5);
    eq(ids(Book::search('')->orderBy('year')->get()), [4, 3, 1, 2]);
});
check('scout:delete-index', function () use ($start) {
    artisan('scout:delete-index', 'books');
    eq(settle($start), []);
    try {
        client()->getIndex(INDEX);
    } catch (Meilisearch\Exceptions\ApiException $e) {
        eq($e->errorCode, 'index_not_found');

        return;
    }
    throw new RuntimeException('the index is still there');
});
check('scout:delete-all-indexes keeps other prefixes', function () use ($start) {
    client()->waitForTask(client()->createIndex('laravel_tmp')['taskUid']);
    client()->waitForTask(client()->createIndex('other_tmp')['taskUid']);
    artisan('scout:delete-all-indexes');
    settle();
    $left = array_map(fn ($index) => $index->getUid(), client()->getIndexes()->getResults());
    truthy(!in_array('laravel_tmp', $left, true), 'laravel_tmp was not deleted');
    truthy(in_array('other_tmp', $left, true), 'other_tmp was deleted');
    client()->waitForTask(client()->deleteIndex('other_tmp')['taskUid']);
});

echo "\n{$passed} passed, ".count($failures)." failed\n";
if ($failures) {
    echo 'failed: '.implode(' | ', $failures)."\n";
    exit(1);
}
