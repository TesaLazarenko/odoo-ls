from typing import Generic, TypeVar

T = TypeVar("T")

class Known(Generic[T]):
    pass

class Test(Unknown[int]):
    pass

class Test2(missing.Base[T]):
    pass
