<?php

namespace App\Models;

use Illuminate\Database\Eloquent\Model;
use Laravel\Scout\Searchable;

class Book extends Model
{
    use Searchable;

    public $timestamps = false;

    protected $guarded = [];

    protected function casts(): array
    {
        return [
            'year' => 'integer',
            'genres' => 'array',
            'price' => 'float',
            'in_print' => 'boolean',
        ];
    }

    /**
     * The document Scout sends to the search server.
     *
     * @return array<string, mixed>
     */
    public function toSearchableArray(): array
    {
        return [
            'id' => $this->id,
            'title' => $this->title,
            'author' => $this->author,
            'year' => $this->year,
            'genres' => $this->genres,
            'price' => $this->price,
            'in_print' => $this->in_print,
            'translator' => $this->translator,
        ];
    }
}
