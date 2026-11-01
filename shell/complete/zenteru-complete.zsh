# zenteru shell completion bridge for zsh (sourced by shell/zenteru.zsh).
#
# zenteru asks the shell for completion candidates by writing the private key
# sequence  ESC [ 9234 ~  to the pty (docs/PROTOCOL.md, "Shell candidates").
# The widget bound to it runs zsh's own completion system on the current
# buffer WITHOUT changing it or printing anything visible, and answers with
#
#   OSC 1337 ; ZenteruCompletions=<base64(JSON)> BEL
#   JSON = {"prefix": <raw word before the cursor>,
#           "candidates": [{"text", "description", "kind"}, ...]}   (max 200)
#
# `text` is the raw (shell-quoted) text that replaces `prefix`.
#
# ESC [ 9235 ~ <base64(before)> ; <base64(after)> LF  asks the same for a
# rewritten line (an IME reading in place of the word at the cursor); the
# real buffer is restored afterwards.
#
# Technique: a `zle -C` completion widget runs _main_complete with `compadd`
# wrapped; the wrapper collects matches (`compadd -A/-D`) instead of adding
# them, so nothing is listed or inserted. The user's compinit/zstyle setup is
# used as is; compinit -C runs (quietly) only if completion was never set up.

[[ -o interactive ]] || return 0
(( ${+_zenteru_cc_loaded} )) && return 0
typeset -g _zenteru_cc_loaded=1

zmodload zsh/zle 2>/dev/null || return 0
zmodload zsh/parameter 2>/dev/null
zmodload zsh/zutil 2>/dev/null   # zparseopts

typeset -gi _zenteru_cc_max=200
typeset -ga _zenteru_cc_json      # JSON objects, one per candidate
typeset -gA _zenteru_cc_seen
typeset -g  _zenteru_cc_capturing=0

# JSON string literal of $1 in REPLY.
_zenteru_cc_jstr() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\t'/\\t}
  s=${s//$'\r'/\\r}
  s=${s//[[:cntrl:]]/}
  REPLY="\"$s\""
}

_zenteru_cc_kind() {
  # $1 = curtag, $2 = is-file flag, $3 = candidate path (unquoted)
  if [[ -n $2 ]]; then
    local p=$3
    [[ $p == '~'* ]] && p=$HOME${p#\~}
    if [[ -d $p ]]; then REPLY=directory; else REPLY=file; fi
    return
  fi
  case $1 in
    commands|builtins|aliases|functions|reserved-words|suffix-aliases|global-aliases|jobs) REPLY=command ;;
    options|*-options) REPLY=option ;;
    directories|local-directories|named-directories|directory-stack|*-directories) REPLY=directory ;;
    files|*-files|paths) REPLY=file ;;
    parameters|*-parameters) REPLY=parameter ;;
    '') REPLY=other ;;
    *) REPLY=$1 ;;
  esac
}

# Stands in for `compadd` while capturing.
_zenteru_cc_compadd() {
  local -A apre hpre dscrs _oad sfx
  local -a isfile isquoted _opts __ expl
  zparseopts -E -a _opts P:=apre p:=hpre d:=dscrs X+:=expl O:=_oad A:=_oad D:=_oad \
    f=isfile Q=isquoted i: S:=sfx s: I: x: r: R: W: F: M+: E: q e n U C \
    J:=__ V:=__ a=__ l=__ k=__ o=__ 1=__ 2=__ 2>/dev/null
  # Internal collection by completion functions (-O/-A/-D): let it through.
  if (( $#_oad )); then
    builtin compadd "$@"
    return
  fi
  local -a __hits __dscr
  if (( $#dscrs == 1 )); then
    __dscr=( "${(@P)${(v)dscrs}}" )
  fi
  builtin compadd -A __hits -D __dscr "$@"
  local ret=$?
  (( $#__hits )) || return $ret

  local i hit desc text cpath kind jt jd jk
  local ap=${(v)apre} hp=${(v)hpre}
  for (( i = 1; i <= $#__hits; i++ )); do
    (( $#_zenteru_cc_json >= _zenteru_cc_max )) && break
    hit=$__hits[i]
    desc=${__dscr[i]-}
    if [[ $desc == *' -- '* ]]; then
      desc=${desc#* -- }
    elif [[ $desc == "$hit" || $desc == "$hit"[[:space:]]# ]]; then
      desc=
    fi
    if (( $#isquoted )); then
      text=$IPREFIX$ap$hp$hit
    else
      text=$IPREFIX${ap:+${(q)ap}}${hp:+${(q)hp}}${(q)hit}
    fi
    if (( $#isquoted )); then cpath=${(Q)text}; else cpath=${(Q)IPREFIX}$ap$hp$hit; fi
    _zenteru_cc_kind "$curtag" "${isfile[1]-}" "$cpath"; kind=$REPLY
    [[ $kind == directory && $text != */ ]] && text+=/
    (( ${+_zenteru_cc_seen[$text]} )) && continue
    _zenteru_cc_seen[$text]=1
    _zenteru_cc_jstr "$text"; jt=$REPLY
    if [[ -n $desc ]]; then _zenteru_cc_jstr "$desc"; jd=$REPLY; else jd=null; fi
    _zenteru_cc_jstr "$kind"; jk=$REPLY
    _zenteru_cc_json+=("{\"text\":$jt,\"description\":$jd,\"kind\":$jk}")
  done
  return 0
}

# The completion widget's function (zle -C): run the completion system with
# compadd wrapped, then make sure nothing is inserted or listed.
_zenteru_cc_main() {
  local saved_compadd=${functions[compadd]-}
  functions[compadd]='_zenteru_cc_compadd "$@"'
  {
    _main_complete 2>/dev/null
  } always {
    if [[ -n $saved_compadd ]]; then
      functions[compadd]=$saved_compadd
    else
      unfunction compadd 2>/dev/null
    fi
  }
  # Add one invisible empty match so completion "succeeds" (no beep, no
  # "no matches" message), and insert/list nothing.
  builtin compadd -U -Q -S '' -- '' 2>/dev/null
  compstate[insert]=''
  compstate[list]=''
  compstate[exact]=''
  compstate[to_end]=''
  return 0
}

_zenteru_cc_ensure_compinit() {
  (( ${+functions[_main_complete]} )) && (( ${+_comps} )) && return 0
  autoload -Uz compinit 2>/dev/null || return 1
  compinit -C -i -d "${ZSH_COMPDUMP:-${ZDOTDIR:-$HOME}/.zcompdump}" >/dev/null 2>&1
  (( ${+functions[_main_complete]} ))
}

# Raw word before the cursor, in REPLY.
_zenteru_cc_rawprefix() {
  local -a w
  w=( ${(z)LBUFFER} )
  REPLY=
  (( $#w )) || return 0
  local last=${w[-1]}
  [[ $LBUFFER == *"$last" ]] && REPLY=$last
  # A lone separator token (|, ;, &&) is not a word.
  [[ $REPLY == ('|'|'||'|'&'|'&&'|';'|';;'|'('|')'|'{'|'}') ]] && REPLY=
}

_zenteru_cc_emit() {
  local jp
  _zenteru_cc_jstr "$1"; jp=$REPLY
  local json="{\"prefix\":$jp,\"candidates\":[${(j:,:)_zenteru_cc_json}]}"
  local b64
  b64=$(print -rn -- "$json" | command base64 | command tr -d '\n')
  print -rn -- $'\e]1337;ZenteruCompletions='"$b64"$'\a' >/dev/tty 2>/dev/null
}

_zenteru_cc_run() {
  # Captures completions for the current BUFFER/CURSOR; emits; restores.
  local buf=$BUFFER cur=$CURSOR
  _zenteru_cc_json=()
  _zenteru_cc_seen=()
  _zenteru_cc_rawprefix
  local prefix=$REPLY
  if _zenteru_cc_ensure_compinit; then
    _zenteru_cc_capturing=1
    zle _zenteru_cc_capture 2>/dev/null
    _zenteru_cc_capturing=0
  fi
  BUFFER=$buf
  CURSOR=$cur
  _zenteru_cc_emit "$prefix"
}

_zenteru_cc_widget() {
  local orig_buf=$BUFFER orig_cur=$CURSOR
  _zenteru_cc_run
  BUFFER=$orig_buf
  CURSOR=$orig_cur
  zle -R 2>/dev/null
}

_zenteru_cc_b64d() {
  print -rn -- "$1" | { command base64 -d 2>/dev/null || command base64 -D 2>/dev/null; }
}

# ESC [ 9235 ~ <base64(before)> ; <base64(after)> LF
_zenteru_cc_query_widget() {
  local c payload= n=0
  while read -k 1 c; do
    [[ $c == $'\n' || $c == $'\r' ]] && break
    payload+=$c
    (( ++n > 65536 )) && break
  done
  local orig_buf=$BUFFER orig_cur=$CURSOR before after
  before=$(_zenteru_cc_b64d "${payload%%;*}")
  after=$(_zenteru_cc_b64d "${payload#*;}")
  [[ $payload == *';'* ]] || after=
  BUFFER=$before$after
  CURSOR=${#before}
  _zenteru_cc_run
  BUFFER=$orig_buf
  CURSOR=$orig_cur
  zle -R 2>/dev/null
}

zle -N _zenteru_cc_widget
zle -N _zenteru_cc_query_widget
zle -C _zenteru_cc_capture complete-word _zenteru_cc_main
# `zle -C` loads zsh/complete, and from then on zsh no longer autoloads the
# old compctl system that plain Tab uses when compinit was never run (no
# compinit in .zshrc, the macOS default). Without this, Tab completes nothing
# in such shells. Load compctl now unless compinit is already set up; a
# compinit run later (in .zshrc, or by the capture above) still takes over.
(( ${+_comps} )) || zmodload -i zsh/compctl 2>/dev/null
() {
  local km
  for km in emacs viins vicmd; do
    bindkey -M $km $'\e[9234~' _zenteru_cc_widget 2>/dev/null
    bindkey -M $km $'\e[9235~' _zenteru_cc_query_widget 2>/dev/null
  done
}
