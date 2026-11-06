# Changes in app from 111111111111 to 222222222222

## Summary

Repository `app`, from `111111111111` to `222222222222`. Facts version `abababababab`. Each list is limited to 200 entries.

| Measure | Value |
| --- | --- |
| Files compared | 5 |
| Symbols added | 2 |
| Symbols removed | 3 |
| Symbols moved | 1 |
| Symbols with a changed signature | 1 |
| Links added | 2 |
| Links removed | 1 |
| Contracts changed | 1 |
| Hubs moved | 1 |
| Cycles | 0 to 1 |
| Communities | 2 to 3 |
| graph.dangling | 0 to 1 |
| graph.links | 10 to 11 |
| graph.symbols | 12 to 13 |

## Symbols

### Added

| Symbol | Kind | Where |
| --- | --- | --- |
| `app:src/new.tx#fn:fresh` | fn | src/new.tx:1 |
| `app:src/lib.tx#fn:extra` | fn | src/lib.tx:12 |

### Removed

| Symbol | Kind | Where |
| --- | --- | --- |
| `app:src/old.tx#fn:gone` | fn | src/old.tx:4 |

Showing 1 of 3.

### Moved

| Symbol | From | To |
| --- | --- | --- |
| `app:src/b.tx#fn:shared` | src/a.tx:2 | src/b.tx:7 |

### Changed

| Symbol | Kind before | Signature before | Signature after |
| --- | --- | --- | --- |
| `app:src/lib.tx#fn:parse` | fn | fn parse(x: u8) | fn parse(x: u8 \| u16) |

## Links

### Added

| From | Kind | To |
| --- | --- | --- |
| `app:src/lib.tx#fn:extra` | calls | `app:src/lib.tx#fn:parse` |
| `app:src/new.tx#fn:fresh` | calls | `app:src/lib.tx#fn:parse` |

### Removed

| From | Kind | To |
| --- | --- | --- |
| `app:src/old.tx#fn:gone` | calls | `app:src/lib.tx#fn:parse` |

### Newly dangling

| From | Kind | Wanted |
| --- | --- | --- |
| `app:src/new.tx#fn:fresh` | links-to | ns:ghost |

## Contracts

### Changed contracts

| Contract | Providers | Consumers | Declarers | Providers added | Providers removed | Consumers added | Consumers removed | Declarers added | Declarers removed | Dangling |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| contract:route:users | 1 to 0 | 1 to 2 | 0 to 0 |  | `app:src/old.tx#fn:gone` | `app:src/new.tx#fn:fresh` |  |  |  | newly dangling |

## Hubs

| Hub | Kind | Degree | Rank |
| --- | --- | --- | --- |
| `app:src/lib.tx#fn:parse` | fn | 4 to 6 | 0.1 to 0.15 |

## Cycles

Cycles: 0 to 1. Nodes in a cycle: 0 to 2.

### New cycles

| Members |
| --- |
| `app:src/a.tx`, `app:src/b.tx` |

## Communities

Communities: 2 to 3. Modularity: 0.31 to 0.42. Communities are matched by label, so a community whose label changed shows as one gone and one new.

### New

| Label | Directory | Size |
| --- | --- | --- |
| app:src/new.tx | src | 3 |

### Resized

| Label | Size before | Size after |
| --- | --- | --- |
| app:src/lib.tx | 4 | 6 |

## Coverage

| Measure | Before | After | Change |
| --- | --- | --- | --- |
| coverage.resolution_rate | 0.5 | 0.75 | 0.25 |

## Limits of the comparison

A rename inside a file is one symbol removed and one added. A change of line number is not a change. A file whose blob is the same on both sides compares as unchanged, even if its context changed. A symbol moves only when exactly one removed and one added symbol share a kind, name and ordinal.

partial: capped at 1 (limit; 3 found); absence is not proof

