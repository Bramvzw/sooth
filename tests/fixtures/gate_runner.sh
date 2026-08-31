#!/bin/sh
# A fake phpunit for the gate contract tests: --version prints the banner
# from $SOOTH_TEST_PHPUNIT_VERSION; a real run writes a green report and
# leaves a ran-tests marker so tests can assert whether tests ran at all.
if [ "$1" = "--version" ]; then
  printf '%s\n' "$SOOTH_TEST_PHPUNIT_VERSION"
  exit 0
fi
report=""; prev=""
for a in "$@"; do
  if [ "$prev" = "--log-junit" ]; then report="$a"; fi
  prev="$a"
done
touch ran-tests
printf '<testsuite><testcase classname="gate" name="ok"/></testsuite>' > "$report"
