class CycleError(Exception):
    pass


def toposort(deps):
    """Order the keys of `deps` so every item comes after everything it depends on.

    `deps` maps an item to an iterable of items it depends on. Items that only appear as
    dependencies are part of the result too. Among items that are ready at the same time the
    alphabetically smallest comes first, so the result is deterministic.

    On a cycle raise CycleError whose message is `cycle: a -> b -> c -> a`, listing the cycle
    starting from its alphabetically smallest member. A self-dependency is a cycle (`cycle: a -> a`).
    """
    raise NotImplementedError
