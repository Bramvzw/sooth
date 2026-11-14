import os


def test_steady():
    assert 1 + 1 == 2


def test_wobbly():
    """Fails on every second invocation: the counter lives outside the tree."""
    path = os.environ["SMOKE_COUNTER"]
    seen = int(open(path).read()) if os.path.exists(path) else 0
    with open(path, "w") as counter:
        counter.write(str(seen + 1))
    assert seen % 2 == 0


def test_broken():
    assert "Your order" == "Your receipt"
