#!/bin/sh
# A fake Pest/PHPUnit for the --verify selection tests. Its one test is the
# method $SOOTH_TEST_METHOD of Tests\Unit\ExampleTest. It always fails, and
# is reported under $SOOTH_TEST_NAME (XML-escaped) — Pest prettifies the
# name of a class-based method. A --filter is applied the way PHPUnit
# applies it: as a regex against `Class::method`, here by perl (PCRE, like
# PHP's preg_match). A filter that misses leaves an empty report.
report=""; filter=""; prev=""
for a in "$@"; do
  if [ "$prev" = "--log-junit" ]; then report="$a"; fi
  if [ "$prev" = "--filter" ]; then filter="$a"; fi
  prev="$a"
done
if [ -n "$filter" ]; then
  SUBJECT="Tests\\Unit\\ExampleTest::$SOOTH_TEST_METHOD" FILTER="$filter" perl -e '
    my ($re, $flags) = $ENV{FILTER} =~ m{^/(.*)/([a-z]*)$}s or exit 1;
    my $compiled = $flags =~ /i/ ? qr/$re/i : qr/$re/;
    exit($ENV{SUBJECT} =~ $compiled ? 0 : 1);
  ' || { printf '<testsuite/>' > "$report"; exit 0; }
fi
printf '<testsuite><testcase classname="Tests.Unit.ExampleTest" name="%s"><failure type="E">no</failure></testcase></testsuite>' "$SOOTH_TEST_NAME" > "$report"
exit 1
