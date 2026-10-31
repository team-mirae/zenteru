# zenteru shell completion bridge for bash (sourced by shell/zenteru.bash).
#
# zenteru writes the private key sequence  ESC [ 9234 ~  to the pty; the
# `bind -x` function below computes completion candidates for the current
# readline buffer WITHOUT changing it or printing anything visible, and
# answers with
#
#   OSC 1337 ; ZenteruCompletions=<base64(JSON)> BEL
#   JSON = {"prefix": <raw word before the cursor>,
#           "candidates": [{"text", "description", "kind"}, ...]}   (max 200)
#
# ESC [ 9235 ~ <base64(before)> ; <base64(after)> LF  does the same for a
# rewritten line (an IME reading in place of the word at the cursor).
#
# Candidates come from the command's programmable completion (`complete -p`:
# -F functions, with bash-completion's dynamic loader, -W/-A/... via compgen),
# else commands in command position, else files. Needs bash >= 4
# (READLINE_LINE/READLINE_POINT); older bash (macOS /bin/bash 3.2) answers
# with no candidates at once, so zenteru doesn't wait for its timeout.

case $- in *i*) ;; *) return 0 ;; esac
[ -n "${_zenteru_cc_loaded-}" ] && return 0
_zenteru_cc_loaded=1

_zenteru_cc_max=200

# JSON string literal of $1 in _zenteru_cc_REPLY.
_zenteru_cc_jstr() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\t'/\\t}
  s=${s//$'\r'/\\r}
  s=${s//[[:cntrl:]]/}
  _zenteru_cc_REPLY="\"$s\""
}

_zenteru_cc_emit() {
  # $1 = prefix; candidates in _zenteru_cc_text/_zenteru_cc_kind arrays
  local json i n=${#_zenteru_cc_text[@]} items= jt jk
  _zenteru_cc_jstr "$1"; json="{\"prefix\":$_zenteru_cc_REPLY,\"candidates\":["
  for (( i = 0; i < n; i++ )); do
    _zenteru_cc_jstr "${_zenteru_cc_text[i]}"; jt=$_zenteru_cc_REPLY
    _zenteru_cc_jstr "${_zenteru_cc_kind[i]}"; jk=$_zenteru_cc_REPLY
    items+="${items:+,}{\"text\":$jt,\"description\":null,\"kind\":$jk}"
  done
  json+="$items]}"
  local b64
  b64=$(printf '%s' "$json" | command base64 | command tr -d '\n')
  printf '\033]1337;ZenteruCompletions=%s\007' "$b64" >/dev/tty 2>/dev/null
}

# Shell-quote a candidate for insertion (unless the word opened a quote).
_zenteru_cc_quote() {
  local s=$1 q=$2
  if [ -n "$q" ]; then
    _zenteru_cc_REPLY=$q$s
    return
  fi
  # Backslash-escape shell metacharacters (printf %q mangles UTF-8 in bash 3.2).
  local out= ch i
  for (( i = 0; i < ${#s}; i++ )); do
    ch=${s:i:1}
    case $ch in
      [[:space:]]|[\\\'\"\$\`\!\&\;\|\<\>\(\)\[\]\{\}\*\?\#\~]) out+="\\$ch" ;;
      *) out+=$ch ;;
    esac
  done
  _zenteru_cc_REPLY=$out
}

_zenteru_cc_add() {
  # $1 = candidate (unquoted), $2 = kind, $3 = open quote char, $4 = path prefix to keep
  [ ${#_zenteru_cc_text[@]} -ge $_zenteru_cc_max ] && return 1
  local c=$1 kind=$2
  if [ "$kind" = file ] || [ "$kind" = directory ]; then
    local p=$c
    case $p in '~'*) p=$HOME${p#\~} ;; esac
    if [ -d "$p" ]; then kind=directory; else kind=file; fi
  fi
  _zenteru_cc_quote "$c" "$3"
  local text=$_zenteru_cc_REPLY
  [ "$kind" = directory ] && [ "${text%/}" = "$text" ] && text+=/
  local t
  for t in "${_zenteru_cc_text[@]}"; do [ "$t" = "$text" ] && return 0; done
  _zenteru_cc_text+=("$text")
  _zenteru_cc_kind+=("$kind")
}

_zenteru_cc_compute() {
  # $1 = line, $2 = point (characters)
  local line=$1 point=$2
  local before=${line:0:point}
  _zenteru_cc_text=()
  _zenteru_cc_kind=()

  # Words (whitespace split; good enough for completion context).
  local -a words
  read -r -a words <<<"$before"
  local cur= prefix=
  case $before in
    *[[:space:]]) cur= ;;
    *) [ ${#words[@]} -gt 0 ] && cur=${words[${#words[@]}-1]} ;;
  esac
  prefix=$cur
  [ -z "$cur" ] && words+=("")
  local cword=$(( ${#words[@]} - 1 ))
  # The current simple command starts after the last ; | & ( operator word.
  local start=0 i
  for (( i = 0; i < cword; i++ )); do
    case ${words[i]} in
      ';'|'|'|'||'|'&'|'&&'|'('|'$('|'`') start=$(( i + 1 )) ;;
    esac
  done
  # Skip assignments and precommands.
  while [ $start -lt $cword ]; do
    case ${words[start]} in
      *=*|sudo|env|time|nohup|exec|command|builtin|nice|xargs) start=$(( start + 1 )) ;;
      *) break ;;
    esac
  done

  # An open quote: complete its contents; candidates keep the quote char.
  local q= ucur=$cur
  case $cur in
    \"*) q=\"; ucur=${cur#\"} ;;
    \'*) q=\'; ucur=${cur#\'} ;;
    *) ucur=${cur//\\/} ;;
  esac
  [ "${ucur:0:1}" = '~' ] && ucur=$HOME${ucur:1} && local tilde=1

  local c
  if [ $cword -eq $start ]; then
    case $ucur in
      */*) while IFS= read -r c; do _zenteru_cc_add "$c" file "$q" || break; done < <(compgen -f -- "$ucur" 2>/dev/null) ;;
      *) while IFS= read -r c; do _zenteru_cc_add "$c" command "$q" || break; done \
           < <(compgen -A function -abck -- "$ucur" 2>/dev/null | LC_ALL=C sort -u) ;;
    esac
  else
    local cmd=${words[start]} prev=${words[cword-1]}
    local spec
    spec=$(complete -p -- "$cmd" 2>/dev/null) || spec=
    if [ -z "$spec" ] && complete -p -D >/dev/null 2>&1; then
      # bash-completion's dynamic loader (complete -D): load, then look again.
      local dfunc
      dfunc=$(complete -p -D 2>/dev/null | sed -n 's/.*-F \([^ ]*\).*/\1/p')
      if [ -n "$dfunc" ]; then
        local COMP_WORDS=("${words[@]:start}") COMP_CWORD=$(( cword - start )) COMP_LINE=$line COMP_POINT=$point
        local -a COMPREPLY=()
        "$dfunc" "$cmd" "$cur" "$prev" >/dev/null 2>&1
        spec=$(complete -p -- "$cmd" 2>/dev/null) || spec=
      fi
    fi
    local func= opts= nospace=
    if [ -n "$spec" ]; then
      eval "local -a sp=($spec)" 2>/dev/null
      local -a cg=()
      local j=1 n=$(( ${#sp[@]} - 1 ))  # last element is the command name
      while [ $j -lt $n ]; do
        case ${sp[j]} in
          -F) func=${sp[j+1]}; j=$(( j + 2 )) ;;
          -C) j=$(( j + 2 )) ;;   # external commands: not run from here
          -o) opts+=" ${sp[j+1]}"; case ${sp[j+1]} in default|bashdefault|dirnames|plusdirs|filenames) cg+=(-o "${sp[j+1]}") ;; esac; j=$(( j + 2 )) ;;
          -[AGWXPS]) cg+=("${sp[j]}" "${sp[j+1]}"); j=$(( j + 2 )) ;;
          -*) cg+=("${sp[j]}"); j=$(( j + 1 )) ;;
          *) j=$(( j + 1 )) ;;
        esac
      done
      local kind=other
      case " $opts " in *" filenames "*|*" dirnames "*|*" default "*) kind=file ;; esac
      if [ -n "$func" ] && declare -F "$func" >/dev/null 2>&1; then
        local COMP_WORDS=("${words[@]:start}") COMP_CWORD=$(( cword - start ))
        local COMP_LINE=$line COMP_POINT=$point COMP_TYPE=9 COMP_KEY=9
        local -a COMPREPLY=()
        "$func" "$cmd" "$cur" "$prev" >/dev/null 2>&1 </dev/null
        for c in "${COMPREPLY[@]}"; do
          case $kind in file) _zenteru_cc_add "$c" file "$q" || break ;; *) _zenteru_cc_add "${c%% }" other "$q" || break ;; esac
        done
      fi
      if [ ${#cg[@]} -gt 0 ]; then
        while IFS= read -r c; do _zenteru_cc_add "$c" "$kind" "$q" || break; done < <(compgen "${cg[@]}" -- "$ucur" 2>/dev/null)
      fi
    fi
    if [ ${#_zenteru_cc_text[@]} -eq 0 ]; then
      while IFS= read -r c; do _zenteru_cc_add "$c" file "$q" || break; done < <(compgen -f -- "$ucur" 2>/dev/null)
    fi
  fi
  if [ -n "${tilde-}" ]; then
    local k
    for k in "${!_zenteru_cc_text[@]}"; do
      case ${_zenteru_cc_text[k]} in "$HOME"*) _zenteru_cc_text[k]="~${_zenteru_cc_text[k]#$HOME}" ;; esac
    done
  fi
  _zenteru_cc_prefix=$prefix
}

_zenteru_cc_bash() {
  if [ -z "${READLINE_LINE+x}" ]; then
    _zenteru_cc_text=(); _zenteru_cc_kind=()
    _zenteru_cc_emit ""
    return 0
  fi
  local saved_line=$READLINE_LINE saved_point=$READLINE_POINT
  # READLINE_POINT counts bytes in older bash, characters in newer: a point
  # equal to the line's byte length is the end of the line either way.
  local point=$READLINE_POINT nbytes
  nbytes=$(printf '%s' "$READLINE_LINE" | LC_ALL=C wc -c | tr -d ' ')
  [ "$point" -ge "$nbytes" ] && point=${#READLINE_LINE}
  _zenteru_cc_compute "$READLINE_LINE" "$point"
  READLINE_LINE=$saved_line
  READLINE_POINT=$saved_point
  _zenteru_cc_emit "$_zenteru_cc_prefix"
}

_zenteru_cc_b64d() {
  printf '%s' "$1" | { command base64 -d 2>/dev/null || command base64 -D 2>/dev/null; }
}

_zenteru_cc_bash_query() {
  local payload
  IFS= read -r payload
  payload=${payload%$'\r'}
  local before after
  before=$(_zenteru_cc_b64d "${payload%%;*}")
  case $payload in *';'*) after=$(_zenteru_cc_b64d "${payload#*;}") ;; *) after= ;; esac
  if [ -z "${READLINE_LINE+x}" ]; then
    _zenteru_cc_text=(); _zenteru_cc_kind=()
    _zenteru_cc_emit ""
    return 0
  fi
  local saved_line=$READLINE_LINE saved_point=$READLINE_POINT
  _zenteru_cc_compute "$before$after" "${#before}"
  READLINE_LINE=$saved_line
  READLINE_POINT=$saved_point
  _zenteru_cc_emit "$_zenteru_cc_prefix"
}

if [ "${BASH_VERSINFO[0]:-0}" -ge 4 ]; then
  bind -x '"\e[9234~": _zenteru_cc_bash' 2>/dev/null
  bind -x '"\e[9235~": _zenteru_cc_bash_query' 2>/dev/null
else
  # bash 3.2 can't run `bind -x` commands on ESC sequences ("cannot find
  # keymap for command"): route them through a macro to a C-x chord.
  bind -x '"\C-x\C-_": _zenteru_cc_bash' 2>/dev/null
  bind -x '"\C-x\C-]": _zenteru_cc_bash_query' 2>/dev/null
  bind '"\e[9234~": "\C-x\C-_"' 2>/dev/null
  bind '"\e[9235~": "\C-x\C-]"' 2>/dev/null
fi
