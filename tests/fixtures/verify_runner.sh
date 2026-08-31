#!/bin/sh
# A fake phpunit for the --verify contract tests: the initial run fails
# c::wob and exits 1; a --filter invocation (verify's selection re-run)
# writes $SOOTH_TEST_VERIFY_CASE instead and exits 0.
report=""; prev=""; verify=0
for a in "$@"; do
  if [ "$prev" = "--log-junit" ]; then report="$a"; fi
  case "$a" in --filter) verify=1;; esac
  prev="$a"
done
if [ "$verify" = "1" ]; then
  printf '<testsuite>%s</testsuite>' "$SOOTH_TEST_VERIFY_CASE" > "$report"
  exit 0
fi
printf '<testsuite><testcase classname="c" name="wob"><failure/></testcase></testsuite>' > "$report"
exit 1
