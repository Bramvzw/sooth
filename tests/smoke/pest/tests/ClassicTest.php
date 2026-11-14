<?php

use PHPUnit\Framework\TestCase;

/** A class-based test in a Pest project: Pest reports it as "Totals add up". */
final class ClassicTest extends TestCase
{
    public function test_totals_add_up(): void
    {
        $this->assertSame(3, 1 + 1);
    }
}
