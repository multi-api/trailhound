<?php

use Illuminate\Database\Migrations\Migration;
use Illuminate\Database\Schema\Blueprint;
use Illuminate\Support\Facades\Schema;

return new class extends Migration
{
    public function up(): void
    {
        Schema::create('books', function (Blueprint $table) {
            $table->id();
            $table->string('title');
            $table->string('author');
            $table->integer('year');
            $table->json('genres');
            $table->float('price');
            $table->boolean('in_print')->default(true);
            $table->string('translator')->nullable();
        });
    }

    public function down(): void
    {
        Schema::dropIfExists('books');
    }
};
