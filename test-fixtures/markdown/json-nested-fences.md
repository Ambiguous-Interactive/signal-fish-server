# JSON Validation Test: Nested Fences

This file tests that the JSON validator correctly skips inner
` ```json` blocks nested inside 4-backtick fences.

## Normal JSON (should be validated)

```json
{
  "valid": true,
  "key": "value"
}
```

## Intentional bad JSON inside 4-backtick fence (should be skipped)

````markdown
```json
{
  // This comment is invalid JSON but should NOT be flagged
  "key": "value"
}
```
````

## Another normal JSON (should be validated)

```json
{
  "items": [1, 2, 3],
  "count": 3
}
```

## 5-backtick fence with nested invalid JSON (should be skipped)

`````text
```json
{
  "items": [...]
}
```
`````

## Final valid JSON (should be validated)

```json
{
  "name": "test",
  "version": "1.0.0"
}
```

Expected: Only 3 JSON blocks should be validated, all valid.
The 2 inner blocks inside 4+ backtick fences should be skipped.
