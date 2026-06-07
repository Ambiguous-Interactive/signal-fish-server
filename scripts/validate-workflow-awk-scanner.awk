# Signal Fish Server - Workflow AWK anti-pattern scanner
#
# Emits tab-delimited findings for scripts/validate-workflow-awk.sh:
#   MATCH_LINES<TAB>line[, line...]
#   NUL_PRINTF
#   COMPLEX<TAB>start_line<TAB>line_count

BEGIN {
  awk_command_names["awk"] = 1
  awk_command_names["gawk"] = 1
  awk_command_names["mawk"] = 1
  awk_command_names["nawk"] = 1
}

function has_nearby_awk_comment(    i, idx) {
  for (i = 0; i < prev_seen; i++) {
    idx = (prev_pos - 1 - i + 5) % 5
    if (prev[idx] ~ /#.*AWK/) {
      return 1
    }
  }
  return 0
}

function remember_previous_line(line) {
  prev[prev_pos] = line
  prev_pos = (prev_pos + 1) % 5
  if (prev_seen < 5) {
    prev_seen++
  }
}

function last_significant_char(text,    i, char) {
  for (i = length(text); i >= 1; i--) {
    char = substr(text, i, 1)
    if (char !~ /[[:space:]]/) {
      return char
    }
  }
  return ""
}

function last_significant_token(text,    i, char, token) {
  token = ""
  for (i = length(text); i >= 1; i--) {
    char = substr(text, i, 1)
    if (char ~ /[[:alnum:]_]/) {
      token = char token
      continue
    }
    if (token != "") {
      return token
    }
    if (char !~ /[[:space:]]/) {
      return ""
    }
  }
  return token
}

function starts_regex_literal(text,    prev, token) {
  prev = last_significant_char(text)
  if (prev == "" || index("~(,=:{;![?+-*%&|", prev) > 0) {
    return 1
  }
  token = last_significant_token(text)
  return token == "print" || token == "printf" || token == "return"
}

function strip_awk_comment(line,    i, char, out, in_string, in_regex, escaped) {
  out = ""
  for (i = 1; i <= length(line); i++) {
    char = substr(line, i, 1)
    if (escaped) {
      out = out char
      escaped = 0
      continue
    }
    if (char == "\\") {
      out = out char
      escaped = 1
      continue
    }
    if (in_string) {
      if (char == "\"") {
        in_string = 0
      }
      out = out char
      continue
    }
    if (in_regex) {
      if (char == "/") {
        in_regex = 0
      }
      out = out char
      continue
    }
    if (char == "\"") {
      in_string = 1
      out = out char
      continue
    }
    if (char == "/" && starts_regex_literal(out)) {
      in_regex = 1
      out = out char
      continue
    }
    if (char == "#") {
      return out
    }
    out = out char
  }
  return out
}

function remember_line(line_number) {
  if (count < 3) {
    if (lines != "") {
      lines = lines ", "
    }
    lines = lines line_number
  }
  count++
}

function reset_match_call() {
  in_match_call = 0
  match_depth = 0
  match_commas = 0
  match_line = 0
  match_in_string = 0
  match_in_regex = 0
  match_escaped = 0
}

function reset_match_search() {
  search_in_string = 0
  search_in_regex = 0
  search_escaped = 0
}

function is_identifier_char(char) {
  return char ~ /[[:alnum:]_]/
}

function match_open_at(line, pos,    cursor) {
  if (substr(line, pos, 5) != "match") {
    return 0
  }
  if (pos > 1 && is_identifier_char(substr(line, pos - 1, 1))) {
    return 0
  }
  cursor = pos + 5
  while (cursor <= length(line) && substr(line, cursor, 1) ~ /[[:space:]]/) {
    cursor++
  }
  if (substr(line, cursor, 1) == "(") {
    return cursor
  }
  return 0
}

function scan_match_calls(line,    i, open_pos, char) {
  i = 1
  while (i <= length(line)) {
    if (!in_match_call) {
      char = substr(line, i, 1)
      if (search_escaped) {
        search_escaped = 0
        i++
        continue
      }
      if (char == "\\") {
        search_escaped = 1
        i++
        continue
      }
      if (search_in_string) {
        if (char == "\"") {
          search_in_string = 0
        }
        i++
        continue
      }
      if (search_in_regex) {
        if (char == "/") {
          search_in_regex = 0
        }
        i++
        continue
      }
      if (char == "\"") {
        search_in_string = 1
        i++
        continue
      }
      if (char == "/" && starts_regex_literal(substr(line, 1, i - 1))) {
        search_in_regex = 1
        i++
        continue
      }
      open_pos = match_open_at(line, i)
      if (!open_pos) {
        i++
        continue
      }
      in_match_call = 1
      match_depth = 1
      match_commas = 0
      match_line = NR
      match_in_string = 0
      match_in_regex = 0
      match_escaped = 0
      i = open_pos + 1
      continue
    }

    char = substr(line, i, 1)
    if (match_escaped) {
      match_escaped = 0
      i++
      continue
    }
    if (char == "\\") {
      match_escaped = 1
      i++
      continue
    }
    if (match_in_string) {
      if (char == "\"") {
        match_in_string = 0
      }
      i++
      continue
    }
    if (match_in_regex) {
      if (char == "/") {
        match_in_regex = 0
      }
      i++
      continue
    }
    if (char == "\"") {
      match_in_string = 1
      i++
      continue
    }
    if (char == "/" && starts_regex_literal(substr(line, 1, i - 1))) {
      match_in_regex = 1
      i++
      continue
    }
    if (char == "(") {
      match_depth++
      i++
      continue
    }
    if (char == ")") {
      match_depth--
      if (match_depth == 0) {
        if (match_commas >= 2) {
          remember_line(match_line)
        }
        reset_match_call()
      }
      i++
      continue
    }
    if (char == "," && match_depth == 1) {
      match_commas++
    }
    i++
  }
}

function has_nul_printf_format(line,    i, j, char, before, after, next_pos, in_string, in_regex, escaped, format_escaped) {
  i = 1
  while (i <= length(line)) {
    char = substr(line, i, 1)
    if (escaped) {
      escaped = 0
      i++
      continue
    }
    if (char == "\\") {
      escaped = 1
      i++
      continue
    }
    if (in_string) {
      if (char == "\"") {
        in_string = 0
      }
      i++
      continue
    }
    if (in_regex) {
      if (char == "/") {
        in_regex = 0
      }
      i++
      continue
    }
    if (char == "\"") {
      in_string = 1
      i++
      continue
    }
    if (char == "/" && starts_regex_literal(substr(line, 1, i - 1))) {
      in_regex = 1
      i++
      continue
    }
    if (substr(line, i, 6) != "printf") {
      i++
      continue
    }

    before = (i == 1) ? "" : substr(line, i - 1, 1)
    after = substr(line, i + 6, 1)
    if ((before != "" && is_identifier_char(before)) ||
        (after != "" && is_identifier_char(after))) {
      i += 6
      continue
    }

    next_pos = i + 6
    while (next_pos <= length(line) && substr(line, next_pos, 1) ~ /[[:space:]]/) {
      next_pos++
    }
    if (substr(line, next_pos, 1) == "(") {
      next_pos++
      while (next_pos <= length(line) && substr(line, next_pos, 1) ~ /[[:space:]]/) {
        next_pos++
      }
    }
    if (substr(line, next_pos, 1) != "\"") {
      i += 6
      continue
    }

    format_escaped = 0
    for (j = next_pos + 1; j <= length(line); j++) {
      char = substr(line, j, 1)
      if (format_escaped) {
        if (char == "0") {
          return 1
        }
        format_escaped = 0
        continue
      }
      if (char == "\\") {
        format_escaped = 1
        continue
      }
      if (char == "\"") {
        break
      }
    }
    i += 6
  }
  return 0
}

function awk_command_length_at(line, pos,    before, after, candidate, len) {
  before = (pos == 1) ? "" : substr(line, pos - 1, 1)
  if (before != "" && before !~ /[[:space:];|&(\/]/) {
    return 0
  }

  for (candidate in awk_command_names) {
    len = length(candidate)
    if (substr(line, pos, len) != candidate) {
      continue
    }
    after = substr(line, pos + len, 1)
    if (after == "" || after ~ /[[:space:]]/) {
      return len
    }
  }

  return 0
}

function env_command_length_at(line, pos,    before, after) {
  if (substr(line, pos, 3) != "env") {
    return 0
  }
  before = (pos == 1) ? "" : substr(line, pos - 1, 1)
  if (before != "" && before !~ /[[:space:];|&(\/]/) {
    return 0
  }
  after = substr(line, pos + 3, 1)
  if (after == "" || after ~ /[[:space:]]/) {
    return 3
  }
  return 0
}

function shell_word_basename(word,    base) {
  base = word
  sub(/^.*\//, "", base)
  return base
}

function strip_shell_word_quotes(word,    quote) {
  quote = substr(word, 1, 1)
  if ((quote == "\047" || quote == "\"") && substr(word, length(word), 1) == quote) {
    return substr(word, 2, length(word) - 2)
  }
  return word
}

function awk_invocation_prefix_ok(line, pos,    prefix, start, i, char, n, token_index) {
  start = 1
  for (i = pos - 1; i >= 1; i--) {
    char = substr(line, i, 1)
    if (is_shell_separator(char)) {
      start = i + 1
      break
    }
  }

  prefix = substr(line, start, pos - start)
  sub(/^[[:space:]]*run:[[:space:]]*/, "", prefix)
  sub(/^[[:space:]]+/, "", prefix)
  sub(/[[:space:]]+$/, "", prefix)
  if (prefix == "") {
    return 1
  }

  delete prefix_tokens
  n = split(prefix, prefix_tokens, /[[:space:]]+/)
  token_index = 1
  if (prefix_tokens[token_index] == "if" ||
      prefix_tokens[token_index] == "while" ||
      prefix_tokens[token_index] == "until") {
    token_index++
    if (prefix_tokens[token_index] == "!") {
      token_index++
    }
  }
  if (prefix_tokens[token_index] == "then" || prefix_tokens[token_index] == "do") {
    token_index++
  }
  while (token_index <= n) {
    if (prefix_tokens[token_index] == "!" || prefix_tokens[token_index] == "{" ||
        prefix_tokens[token_index] == "(") {
      token_index++
      continue
    }
    if (prefix_tokens[token_index] ~ /^[A-Za-z_][A-Za-z0-9_]*=/) {
      token_index++
      continue
    }
    if (prefix_tokens[token_index] == "time") {
      token_index++
      while (token_index <= n && prefix_tokens[token_index] ~ /^-/) {
        token_index++
      }
      continue
    }
    if (prefix_tokens[token_index] == "command") {
      token_index++
      while (token_index <= n && prefix_tokens[token_index] ~ /^-/) {
        token_index++
      }
      continue
    }
    if (shell_word_basename(prefix_tokens[token_index]) == "env") {
      token_index++
      while (token_index <= n) {
        if (prefix_tokens[token_index] == "-u" ||
            prefix_tokens[token_index] == "--unset" ||
            prefix_tokens[token_index] == "-C" ||
            prefix_tokens[token_index] == "--chdir") {
          token_index += 2
          continue
        }
        if (prefix_tokens[token_index] ~ /^--(unset|chdir)=/) {
          token_index++
          continue
        }
        if (prefix_tokens[token_index] ~ /^-[uC]./) {
          token_index++
          continue
        }
        if (prefix_tokens[token_index] == "-S" ||
            prefix_tokens[token_index] == "--split-string") {
          token_index += 2
          continue
        }
        if (prefix_tokens[token_index] ~ /^--split-string=/) {
          token_index++
          continue
        }
        if (prefix_tokens[token_index] ~ /^-/ ||
            prefix_tokens[token_index] ~ /^[A-Za-z_][A-Za-z0-9_]*=/) {
          token_index++
          continue
        }
        break
      }
      continue
    }
    if (token_index == n && prefix_tokens[token_index] ~ /\/$/) {
      token_index++
      continue
    }
    break
  }
  return token_index > n
}

function skip_spaces(line, pos) {
  while (pos <= length(line) && substr(line, pos, 1) ~ /[[:space:]]/) {
    pos++
  }
  return pos
}

function is_shell_separator(char) {
  return char == ";" || char == "|" || char == "&" || char == ")"
}

function find_closing_quote(line, pos, quote,    i, char, escaped) {
  for (i = pos; i <= length(line); i++) {
    char = substr(line, i, 1)
    if (quote == "\"" && escaped) {
      escaped = 0
      continue
    }
    if (quote == "\"" && char == "\\") {
      escaped = 1
      continue
    }
    if (char == quote) {
      return i
    }
  }
  return 0
}

function skip_shell_word(line, pos,    i, char, in_single, in_double, escaped) {
  for (i = pos; i <= length(line); i++) {
    char = substr(line, i, 1)
    if (escaped) {
      escaped = 0
      continue
    }
    if (char == "\\" && !in_single) {
      escaped = 1
      continue
    }
    if (char == "\047" && !in_double) {
      in_single = !in_single
      continue
    }
    if (char == "\"" && !in_single) {
      in_double = !in_double
      continue
    }
    if (!in_single && !in_double && (char ~ /[[:space:]]/ || is_shell_separator(char))) {
      return i
    }
  }
  return length(line) + 1
}

function skip_shell_command(line, pos,    i, char, in_single, in_double, escaped) {
  for (i = pos; i <= length(line); i++) {
    char = substr(line, i, 1)
    if (escaped) {
      escaped = 0
      continue
    }
    if (char == "\\" && !in_single) {
      escaped = 1
      continue
    }
    if (char == "\047" && !in_double) {
      in_single = !in_single
      continue
    }
    if (char == "\"" && !in_single) {
      in_double = !in_double
      continue
    }
    if (!in_single && !in_double && is_shell_separator(char)) {
      return i + 1
    }
  }
  return length(line) + 1
}

function scan_shell_word_nested_groups(line, pos, word_end,    word) {
  if (word_end <= pos) {
    return
  }
  word = substr(line, pos, word_end - pos)
  process_shell_line(word, 1)
}

function skip_shell_word_scanning_nested_groups(line, pos,    word_end) {
  word_end = skip_shell_word(line, pos)
  scan_shell_word_nested_groups(line, pos, word_end)
  return word_end
}

function handle_env_split_command(line, env_pos, command_len,    i, word_end, word, split_command, rest) {
  i = env_pos + command_len
  while (i <= length(line)) {
    i = skip_spaces(line, i)
    if (i > length(line)) {
      return -1
    }
    word_end = skip_shell_word(line, i)
    word = substr(line, i, word_end - i)

    if (word == "-u" || word == "--unset" || word == "-C" || word == "--chdir") {
      i = skip_spaces(line, word_end)
      i = skip_shell_word(line, i)
      continue
    }
    if (word ~ /^--(unset|chdir)=/ || word ~ /^-[uC]./) {
      i = word_end
      continue
    }
    if (word == "-S" || word == "--split-string") {
      i = skip_spaces(line, word_end)
      word_end = skip_shell_word(line, i)
      split_command = strip_shell_word_quotes(substr(line, i, word_end - i))
      rest = substr(line, word_end)
      process_shell_line(split_command " " rest, 1)
      return in_awk ? 0 : length(line) + 1
    }
    if (word ~ /^--split-string=/) {
      split_command = strip_shell_word_quotes(substr(word, index(word, "=") + 1))
      rest = substr(line, word_end)
      process_shell_line(split_command " " rest, 1)
      return in_awk ? 0 : length(line) + 1
    }
    return -1
  }
  return -1
}

function find_group_end(line, pos,    i, char, depth, in_single, in_double, escaped) {
  depth = 1
  for (i = pos; i <= length(line); i++) {
    char = substr(line, i, 1)
    if (escaped) {
      escaped = 0
      continue
    }
    if (char == "\\" && !in_single) {
      escaped = 1
      continue
    }
    if (char == "\047" && !in_double) {
      in_single = !in_single
      continue
    }
    if (char == "\"" && !in_single) {
      in_double = !in_double
      continue
    }
    if (!in_single && substr(line, i, 2) == "$(") {
      depth++
      i++
      continue
    }
    if (!in_single && (substr(line, i, 2) == "<(" || substr(line, i, 2) == ">(")) {
      depth++
      i++
      continue
    }
    if (!in_single && !in_double && char == ")") {
      depth--
      if (depth == 0) {
        return i
      }
    }
  }
  return 0
}

function scan_awk_line(line,    code_line) {
  awk_line_count++
  code_line = strip_awk_comment(line)
  if (code_line !~ /^[[:space:]]*$/) {
    scan_match_calls(code_line)
  }
  if (has_nul_printf_format(code_line)) {
    nul_found = 1
  }
}

function start_awk() {
  in_awk = 1
  awk_start = NR
  awk_line_count = 0
  has_comment = has_nearby_awk_comment()
  reset_match_search()
}

function finish_awk() {
  if (in_awk && awk_line_count > 15 && !has_comment) {
    print "COMPLEX\t" awk_start "\t" awk_line_count
  }
  in_awk = 0
  awk_line_count = 0
  awk_quote = ""
  reset_match_call()
  reset_match_search()
}

function scan_quoted_awk_program(line, quote_pos, quote,    close_pos, body) {
  start_awk()
  awk_quote = quote
  close_pos = find_closing_quote(line, quote_pos + 1, quote)
  if (close_pos) {
    body = substr(line, quote_pos + 1, close_pos - quote_pos - 1)
    scan_awk_line(body)
    finish_awk()
    return close_pos + 1
  }
  body = substr(line, quote_pos + 1)
  scan_awk_line(body)
  return 0
}

function scan_awk_source_operand(line, source_pos, word_end,    char, source) {
  if (source_pos > length(line)) {
    return source_pos
  }
  char = substr(line, source_pos, 1)
  if (char == "\047" || char == "\"") {
    return scan_quoted_awk_program(line, source_pos, char)
  }
  if (word_end <= source_pos) {
    return word_end
  }
  source = substr(line, source_pos, word_end - source_pos)
  start_awk()
  scan_awk_line(source)
  finish_awk()
  return word_end
}

function scan_w_option_operand(line, source_pos, word_end,    word, value_pos) {
  word = substr(line, source_pos, word_end - source_pos)
  if (word ~ /^source=/) {
    value_pos = source_pos + index(word, "=")
    return scan_awk_source_operand(line, value_pos, word_end)
  }
  if (word == "source") {
    source_pos = skip_spaces(line, word_end)
    word_end = skip_shell_word(line, source_pos)
    return scan_awk_source_operand(line, source_pos, word_end)
  }
  if (word ~ /^exec=/ || word == "exec") {
    if (word == "exec") {
      source_pos = skip_spaces(line, word_end)
      word_end = skip_shell_word(line, source_pos)
    }
    scan_shell_word_nested_groups(line, source_pos, word_end)
    return word_end
  }
  scan_shell_word_nested_groups(line, source_pos, word_end)
  return word_end
}

function is_no_operand_awk_option(word) {
  if (word ~ /^--(debug|dump-variables|gen-pot|lint|pretty-print|profile)(=|$)/) {
    return 1
  }
  if (word ~ /^-[DdgILop]./) {
    return 1
  }
  return word == "--bignum" ||
    word == "--characters-as-bytes" ||
    word == "--copyright" ||
    word == "--debug" ||
    word == "--gen-pot" ||
    word == "--help" ||
    word == "--lint" ||
    word == "--lint-old" ||
    word == "--non-decimal-data" ||
    word == "--optimize" ||
    word == "--posix" ||
    word == "--pretty-print" ||
    word == "--profile" ||
    word == "--re-interval" ||
    word == "--sandbox" ||
    word == "--no-optimize" ||
    word == "--traditional" ||
    word == "--trace" ||
    word == "--use-lc-numeric" ||
    word == "--version" ||
    word ~ /^-[bcCDdghILMNnOoPprSsTtV]$/
}

function handle_awk_command(line, awk_pos, command_len,    i, word_end, word, char) {
  i = awk_pos + command_len
  while (i <= length(line)) {
    i = skip_spaces(line, i)
    if (i > length(line)) {
      return length(line) + 1
    }
    char = substr(line, i, 1)
    if (is_shell_separator(char)) {
      return i + 1
    }
    if (char == "\047" || char == "\"") {
      return scan_quoted_awk_program(line, i, char)
    }

    word_end = skip_shell_word(line, i)
    word = substr(line, i, word_end - i)
    if (word == "-e" || word == "--source") {
      i = skip_spaces(line, word_end)
      word_end = skip_shell_word(line, i)
      i = scan_awk_source_operand(line, i, word_end)
      if (in_awk) {
        return 0
      }
      continue
    }
    if (word ~ /^-e./) {
      i = scan_awk_source_operand(line, i + 2, word_end)
      if (in_awk) {
        return 0
      }
      continue
    }
    if (word ~ /^--source=/) {
      i = scan_awk_source_operand(line, i + index(word, "="), word_end)
      if (in_awk) {
        return 0
      }
      continue
    }
    if (word == "-f" || word == "--file" || word == "-E" || word == "--exec") {
      i = skip_spaces(line, word_end)
      i = skip_shell_word_scanning_nested_groups(line, i)
      if (in_awk) {
        return 0
      }
      continue
    }
    if (word ~ /^-[fE]./ || word ~ /^--(file|exec)=/) {
      scan_shell_word_nested_groups(line, i, word_end)
      if (in_awk) {
        return 0
      }
      i = word_end
      continue
    }
    if (word == "-i" || word == "--include" || word == "-l" || word == "--load") {
      i = skip_spaces(line, word_end)
      i = skip_shell_word_scanning_nested_groups(line, i)
      if (in_awk) {
        return 0
      }
      continue
    }
    if (word ~ /^-[il]./ || word ~ /^--(include|load)=/) {
      scan_shell_word_nested_groups(line, i, word_end)
      if (in_awk) {
        return 0
      }
      i = word_end
      continue
    }
    if (word == "-F" || word == "--field-separator") {
      i = skip_spaces(line, word_end)
      i = skip_shell_word_scanning_nested_groups(line, i)
      if (in_awk) {
        return 0
      }
      continue
    }
    if (word ~ /^-F./ || word ~ /^--field-separator=/) {
      scan_shell_word_nested_groups(line, i, word_end)
      if (in_awk) {
        return 0
      }
      i = word_end
      continue
    }
    if (word == "-v" || word == "--assign" || word == "-W") {
      i = skip_spaces(line, word_end)
      word_end = skip_shell_word(line, i)
      if (word == "-W") {
        i = scan_w_option_operand(line, i, word_end)
      } else {
        scan_shell_word_nested_groups(line, i, word_end)
        i = word_end
      }
      if (in_awk) {
        return 0
      }
      continue
    }
    if (word ~ /^-Wsource=/) {
      i = scan_awk_source_operand(line, i + 2 + index(substr(word, 3), "="), word_end)
      if (in_awk) {
        return 0
      }
      continue
    }
    if (word ~ /^-Wexec=/) {
      scan_shell_word_nested_groups(line, i, word_end)
      if (in_awk) {
        return 0
      }
      i = word_end
      continue
    }
    if (word ~ /^-v./ || word ~ /^--assign=/ || word ~ /^-W./) {
      scan_shell_word_nested_groups(line, i, word_end)
      if (in_awk) {
        return 0
      }
      i = word_end
      continue
    }
    if (is_no_operand_awk_option(word) || word == "--") {
      i = word_end
      continue
    }
    return skip_shell_command(line, word_end)
  }
  return length(line) + 1
}

function process_shell_line(line, pos,    i, char, group_end, group_body, next_pos, command_len, env_len, in_single, in_double, escaped) {
  i = (pos > 0) ? pos : 1
  while (i <= length(line)) {
    char = substr(line, i, 1)
    if (escaped) {
      escaped = 0
      i++
      continue
    }
    if (char == "\\" && !in_single) {
      escaped = 1
      i++
      continue
    }
    if (char == "\047" && !in_double) {
      in_single = !in_single
      i++
      continue
    }
    if (char == "\"" && !in_single) {
      in_double = !in_double
      i++
      continue
    }
    if (!in_single &&
        (substr(line, i, 2) == "$(" || substr(line, i, 2) == "<(" || substr(line, i, 2) == ">(")) {
      group_end = find_group_end(line, i + 2)
      if (group_end) {
        group_body = substr(line, i + 2, group_end - i - 2)
        process_shell_line(group_body, 1)
        if (in_awk) {
          return
        }
        i = group_end + 1
        continue
      }
      group_body = substr(line, i + 2)
      process_shell_line(group_body, 1)
      return
    }
    env_len = (!in_single && !in_double) ? env_command_length_at(line, i) : 0
    if (env_len && awk_invocation_prefix_ok(line, i)) {
      next_pos = handle_env_split_command(line, i, env_len)
      if (next_pos == 0) {
        return
      }
      if (next_pos > 0) {
        i = next_pos
        continue
      }
    }
    command_len = (!in_single && !in_double) ? awk_command_length_at(line, i) : 0
    if (command_len && awk_invocation_prefix_ok(line, i)) {
      next_pos = handle_awk_command(line, i, command_len)
      if (next_pos == 0) {
        return
      }
      i = next_pos
      continue
    }
    i++
  }
}

/^[[:space:]]*#/ {
  remember_previous_line($0)
  next
}

{
  if (in_awk) {
    close_pos = find_closing_quote($0, 1, awk_quote)
    if (close_pos) {
      scan_awk_line(substr($0, 1, close_pos - 1))
      finish_awk()
      process_shell_line(substr($0, close_pos + 1), 1)
    } else {
      scan_awk_line($0)
    }
  } else {
    process_shell_line($0, 1)
  }
  remember_previous_line($0)
}

END {
  finish_awk()
  if (lines != "") {
    print "MATCH_LINES\t" lines
  }
  if (nul_found) {
    print "NUL_PRINTF"
  }
}
