from typing import Dict, Generic, Iterable, Iterator, List, Mapping, Optional, Protocol, Sequence, Type, TypeVar

T = TypeVar("T")
K = TypeVar("K")
V = TypeVar("V")


class Item:
    def ping(self) -> int: ...


class Registry(dict, Generic[K, V]):
    def lookup(self, key: K) -> V: ...


class Box(Generic[T]):
    def __init__(self, value: T): ...

    def get(self) -> T: ...

    def items(self) -> List[T]: ...


class ItemList(List[Item]):
    pass


class Proto(Protocol[T]):
    def take(self) -> T: ...


class StrMap(Mapping[str, Item]):
    pass


class ItemBox(Box[Item]):
    pass


def items() -> list[Item]: ...
def old_items() -> List[Item]: ...
def mapping() -> dict[str, Item]: ...
def old_mapping() -> Dict[str, Item]: ...
def maybe() -> Optional[Item]: ...
def klass() -> type[Item]: ...
def old_klass() -> Type[Item]: ...
def boxed() -> Box[Item]: ...
def registry() -> Registry[str, Item]: ...
def seq() -> Sequence[Item]: ...
def it() -> Iterator[Item]: ...
def nested() -> list[list[Item]]: ...


def params(xs: list[Item], m: dict[str, Item], cls: type[Item], box: Box[Item]):
    xs[0]
    m["a"]
    cls
    box.get()


a = items()[0]
a
b = old_items()[0]
b
for c in items():
    c
d = mapping()["x"]
d
e = mapping().get("x")
e
for f in mapping().values():
    f
g = old_mapping()["x"]
g
h = maybe()
h
i = klass()
i
j = old_klass()
j
k = boxed().get()
k
l = registry().lookup("x")
l
m = ItemList()[0]
m
n = seq()[0]
n
for o in it():
    o
p = next(it())
p
q = nested()[0][0]
q
r = boxed().items()[0]
r
s = ItemBox().get()
s
t = StrMap()["x"]
t
annotated: list[Item] = []
annotated[0]


def first(xs: Iterable[T]) -> T: ...
def pair(key: K, value: V) -> Dict[K, V]: ...
def ident(x: T) -> T: ...


u = first(items())
u
w = pair("a", Item())["a"]
w
x = ident(Item())
x
y = sorted(items())[0]
y
z = mapping().get("x", Item())
z
for (pos, val) in enumerate(items()):
    val
sorted(items())
sr = sorted(items())
sr
from collections.abc import Iterable as AbcIterable
def first2(xs: AbcIterable[T]) -> T: ...
f2 = first2(items())
f2
AbcIterable
for (key, value) in mapping().items():
    key
    value
for name, item in mapping().items():
    item
built = Box(Item())
built.get()
copied = list(items())[0]
copied
en = enumerate(items())
en
nx = next(en)
nx
for e2 in enumerate(items()):
    e2


class Stack[S]:
    def __init__(self, first: S): ...

    def top(self) -> S: ...


class ItemStack(Stack[Item]):
    pass


def last[L](values: list[L]) -> L: ...


s1 = Stack(Item()).top()
s1
s2 = ItemStack(Item()).top()
s2
s3 = last(items())
s3
def stacked() -> Stack[Item]: ...
s4 = stacked().top()
s4
shown = mapping()
shown
def plain_box() -> Box: ...
unbound = plain_box().get()
unbound
def parse_as(type_: type[T], raw: str) -> T: ...
def klass_of(value: T) -> type[T]: ...
parsed = parse_as(Item, "x")
parsed
klass2 = klass_of(Item())
klass2
