# Version precedence
A version is `MAJOR.MINOR.PATCH`, optionally followed by `-PRERELEASE` and/or `+BUILD`.

1. Compare MAJOR, MINOR, PATCH numerically, left to right.
2. Build metadata (everything after `+`) is ignored.
3. A version with a prerelease has LOWER precedence than the same version without one.
4. Two prereleases are compared identifier by identifier (identifiers are separated by `.`):
   - both numeric: compare as integers;
   - both non-numeric: compare as ASCII strings;
   - numeric vs non-numeric: the numeric one is lower.
5. If all shared identifiers are equal, the prerelease with MORE identifiers is higher.
6. Raise ValueError for anything that is not a valid version (missing parts, leading zeros in a
   numeric part such as `01.2.3`, empty prerelease identifier such as `1.0.0-a..b`).
