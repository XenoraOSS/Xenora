# Migration to BetterXen

BetterXen is an independent copy of [XenoraOSS/Xenora](https://github.com/XenoraOSS/Xenora), hosted at [MacosPorters/betterxen](https://github.com/MacosPorters/betterxen). This is a copy, not a GitHub ownership transfer. The source repository remains unchanged.

## Preserved history

Original commits and authorship are retained. All 161 original pull-request heads and 24 available merge refs were copied. The 22 open PRs keep their `archive/pull/<original-number>/head` branches. The other 163 refs are preserved as tags named `archive/pull/<original-number>/head` or `archive/pull/<original-number>/merge`; their branch aliases were removed to avoid recent-push banners for historical work. Every tag was checked against its original commit before its branch alias was removed. The metadata archive also includes patches obtained through GitHub's API.

Issues are recreated with original-source links, author attribution and archived comment text. Original timestamps and authors cannot be assigned to recreated GitHub records. Destination issue and pull-request numbers differ. Historical `#number` references in upstream records refer to the original repository; consult the source link and number maps in the migration archive.

Open PRs are recreated against copied branches. Closed and merged PRs are retained as historical archive records, including descriptions, comments, reviews, changed-file metadata, commits and patches; they are not represented as newly merged work. Recreated PRs do not inherit approvals or check results.

The migration archive was uploaded with the `migration-archive-2026-11-24` release, but that release was no longer listed by GitHub during the branch cleanup. A verified local copy remains at `artifacts/repo-migration/xenora-to-betterxen-archive.zip`; `artifacts/` is ignored by Git. Git history also remains on GitHub through the active PR branches and archive tags. It includes JSON metadata, a readable issue/PR index, patches and original-to-destination number maps. It records the public state observed during migration, not an atomic snapshot. Linked external attachments and unavailable private data are not embedded unless explicitly listed in its manifest.

Stars, watchers, source organization membership, private settings, secrets, deploy keys, inaccessible project data, and Actions run history are not copied. The source wiki could not be cloned (GitHub reported that it was not found). No source releases were present during inventory. The one public discussion is archived and recreated with attribution. The destination was private at the initial inventory and later reported public visibility. The migration commands did not request a visibility change.

## Names and compatibility

The primary executable and Cargo package are `betterxen`. Existing `xenora-*` library package names, C interface symbols, the synthetic CPUID vendor string, environment-variable names and synthetic guest import names are retained for compatibility. Copyright notices, licenses, historical review evidence and upstream attribution retain their original names. Prior hardware and verification claims describe upstream evidence and do not certify this renamed copy.

The destination coordination configuration names `Arths17` as its maintainer. Source-specific contributor routing is cleared; destination maintainers can configure it for their own team.
