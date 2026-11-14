const fs = require('fs');

test('steady', () => {
  expect(1 + 1).toBe(2);
});

// Fails on every second invocation: the counter lives outside the tree.
test('wobbly', () => {
  const file = process.env.SMOKE_COUNTER;
  const seen = fs.existsSync(file) ? Number(fs.readFileSync(file, 'utf8')) : 0;
  fs.writeFileSync(file, String(seen + 1));
  expect(seen % 2).toBe(0);
});

test('broken', () => {
  expect('Your order').toBe('Your receipt');
});
