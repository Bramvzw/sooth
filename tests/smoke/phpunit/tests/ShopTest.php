<?php

use PHPUnit\Framework\TestCase;

final class ShopTest extends TestCase
{
    public function test_steady(): void
    {
        $this->assertSame(2, 1 + 1);
    }

    /** Fails on every second invocation: the counter lives outside the tree. */
    public function test_wobbly(): void
    {
        $file = getenv('SMOKE_COUNTER');
        $seen = (int) @file_get_contents($file);
        file_put_contents($file, (string) ($seen + 1));
        $this->assertSame(0, $seen % 2, 'every second invocation fails');
    }

    public function test_broken(): void
    {
        $this->assertSame('Your receipt', 'Your order');
    }
}
