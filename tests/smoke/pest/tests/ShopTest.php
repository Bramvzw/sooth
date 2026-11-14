<?php

test('steady', function () {
    expect(1 + 1)->toBe(2);
});

// Fails on every second invocation: the counter lives outside the tree.
test('wobbly', function () {
    $file = getenv('SMOKE_COUNTER');
    $seen = (int) @file_get_contents($file);
    file_put_contents($file, (string) ($seen + 1));
    expect($seen % 2)->toBe(0);
});

test('broken', function () {
    expect('Your order')->toBe('Your receipt');
});
