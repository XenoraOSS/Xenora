# Observing package process entry

Use `xenora_loader::package::run_package_startup_with_entry_observer` when
a host integration needs to enable services only during the root executable's
entry attempt. It accepts the same arguments as `run_package_startup` plus a
callback.

`PackageEntryEvent::Entering` supplies the root image index, mapped entry
address and actual main guest TEB. The loader has already completed the
required TLS callbacks and DLL attach. If initialization refuses the package
or ends the process before entry, the observer receives neither event.

`PackageEntryEvent::Leaving` ends the entry attempt before detach callbacks
run. A scheduler refusal still delivers Leaving before the startup error
returns. For completed runs, the usual package result carries execution and
detach evidence. The main TEB can also appear in DLL callbacks, so the TEB
and the active entry phase should both be checked before accepting entry-only
host requests.
