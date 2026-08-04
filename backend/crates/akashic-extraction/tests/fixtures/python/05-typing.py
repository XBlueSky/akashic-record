from typing import TypeVar, Generic, List

T = TypeVar('T')

class Container(Generic[T]):
    def __init__(self):
        self.items: List[T] = []

    def push(self, item: T) -> None:
        self.items.append(item)
