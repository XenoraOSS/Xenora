# COM base services

Xenora's Component Object Model (COM) compatibility is partial. Import
resolution does not mean a COM class or interface is implemented. The normal
runtime has no registered COM classes, general class registry, cross-apartment
marshaling, remote activation, or Windows Runtime activation. An explicitly
opt-in synthetic fixture provides one host-backed object and class factory for
testing; it is not a Windows class. No Xbox or Windows behavior has been
independently verified by this change.

`CoInitialize` and `CoInitializeEx` record a calling thread's selected apartment
model and count successful initializations; `CoUninitialize` balances them.
The scheduler discards that state on thread retirement. Message dispatch,
object teardown and other apartment effects are not modeled.

`CoCreateInstance` and local `CoGetClassObject` return the uninitialized-thread
error or class-not-registered error as applicable. `CoCreateInstanceEx`
accepts at most 64 local `MULTI_QI` entries and also reports an unregistered
class. Xenora rejects nonzero server information and larger requests as
unsupported. Its choice to leave the result array unchanged after class lookup
fails is a Xenora policy: Windows behavior for that case is not established.

`CoCreateGuid` produces a random version-4 GUID. `StringFromGUID2`,
`CLSIDFromString` and `IIDFromString` support GUID text and null input as
specified in their service entries. Programmatic identifier (ProgID) lookup is
not supported. The task allocator uses a separate bounded guest heap.
`PropVariantClear` handles value types and task-allocated narrow and wide
strings; arrays, vectors, by-reference values, BSTRs and interface references
are refused. It does not implement `SysFreeString` or interface `Release`.
This work adds no `oleaut32.dll` automation exports or
`api-ms-win-core-winrt-*` activation exports; those imports remain unsupported.

Original C fixtures exercise the partial services, thread retirement and the
opt-in host factory through translated guest code. The synthetic factory
supports `IUnknown` and `IClassFactory` query, reference counts and creation;
`LockServer` is unsupported. A C++ fixture exercises virtual calls and
reference counts within guest code. These are local synthetic tests, not
independent acceptance or a compatibility claim for commercial applications.
Rebuild or compare the compiled fixtures with the four `build-com-*.sh` scripts
in `fixtures/pe`; run `cargo test --locked -p xenora-loader --test guest_com`
to execute them.

Public contracts: [COM initialization](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-coinitializeex),
[class activation](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-cocreateinstance),
[GUID parsing](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-clsidfromstring),
and [variant clearing](https://learn.microsoft.com/en-us/windows/win32/api/propidl/nf-propidl-propvariantclear).
See the [generated service table](services.md) for registered aliases and
individual limits.
