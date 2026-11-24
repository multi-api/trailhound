<?php

use App\Models\Book;

return [
    'driver' => env('SCOUT_DRIVER', 'meilisearch'),
    'prefix' => env('SCOUT_PREFIX', ''),
    'queue' => env('SCOUT_QUEUE', false),
    'after_commit' => false,
    'chunk' => [
        'searchable' => 500,
        'unsearchable' => 500,
    ],
    'soft_delete' => false,
    'identify' => false,

    'meilisearch' => [
        'host' => env('MEILISEARCH_HOST', 'http://127.0.0.1:7700'),
        'key' => env('MEILISEARCH_KEY'),
        // Applied by `php artisan scout:sync-index-settings`. Scout's `where`
        // and `orderBy` need the attributes they use declared here.
        'index-settings' => [
            Book::class => [
                'filterableAttributes' => ['author', 'year', 'genres', 'price', 'in_print', 'translator'],
                'sortableAttributes' => ['year', 'price'],
            ],
        ],
    ],
];
