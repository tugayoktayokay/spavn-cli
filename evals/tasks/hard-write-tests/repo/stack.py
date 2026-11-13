class Stack:
    """A bounded LIFO stack."""

    def __init__(self, capacity):
        if capacity < 1:
            raise ValueError("capacity must be at least 1")
        self._items = []
        self._capacity = capacity

    def push(self, item):
        if len(self._items) >= self._capacity:
            raise OverflowError("stack is full")
        self._items.append(item)

    def pop(self):
        if not self._items:
            raise IndexError("stack is empty")
        return self._items.pop()

    def peek(self):
        if not self._items:
            raise IndexError("stack is empty")
        return self._items[-1]

    def __len__(self):
        return len(self._items)
