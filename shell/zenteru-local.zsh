# zenteru: run or open files in the current directory by name.
#
#   % deploy.sh --dry-run        # no `deploy.sh` command anywhere
#   zenteru: running ./deploy.sh
#   % Screenshot.png
#   zenteru: opening ./Screenshot.png
#   % projects                   # a folder: becomes `cd -- projects`
#
# Four parts:
#
# 1. A command-not-found handler for bare names (no `/`). It only ever runs
#    when no command, function, alias or builtin of that name exists, so it
#    can't shadow a real command; the tradeoff is that a mistyped command
#    can run a local file of that name. Executables run as ./NAME, other
#    files with their #! interpreter, else .sh with sh, .zsh with zsh and
#    .bash with bash; any other file opens with macOS `open`. Names that
#    aren't files fall through to the handler you had before (Homebrew's
#    command-not-found, say) or "command not found".
# 2. Enter on a line that is only a folder (`projects`, `../x`, `~/src`)
#    turns it into `cd -- FOLDER`, visibly, whatever the AUTO_CD option says,
#    unless a command of that name exists. (The not-found handler runs in a
#    subshell, so it can't cd.)
# 3. Enter on a line whose first word is a path to a file that can't run
#    as is (`./notes.pdf`, `~/x.sh` without +x) puts the interpreter or
#    `open --` in front of it, visibly, instead of "permission denied".
# 4. Tab in the first word offers files and folders as well as commands.

# How to run or open FILE (a path), as command words in reply; status 1 if
# it isn't a regular file. The path is always the last word.
_zenteru_file_plan() {
  local f=$1 first=
  reply=()
  [[ -f $f ]] || return 1
  if [[ -x $f ]]; then
    reply=($f)
    return 0
  fi
  IFS= read -r first < $f 2>/dev/null
  if [[ $first == '#!'* ]]; then
    reply=(${=${first#\#!}} $f)
    (( $#reply > 1 )) && return 0
  fi
  case $f in
    *.sh) reply=(sh $f) ;;
    *.zsh) reply=(zsh $f) ;;
    *.bash) reply=(bash $f) ;;
    *) reply=(open -- $f) ;;
  esac
}

_zenteru_local_handler() {
  local -a reply
  if [[ -n $1 && $1 != */* ]] && _zenteru_file_plan ./$1; then
    shift
    local note="running ${(j: :)${(@q-)reply}}"
    [[ $reply[1] == open ]] && note="opening ${reply[-1]}"
    print -u2 -r -- $'\e[2m'"zenteru: $note"$'\e[22m'
    "${reply[@]}" "$@"
    return
  fi
  if (( ${+functions[_zenteru_cnf_prev]} )); then
    _zenteru_cnf_prev "$@"
    return
  fi
  print -u2 -r -- "zsh: command not found: $1"
  return 127
}

# Enter: a line that is only a folder becomes `cd -- FOLDER`; a first word
# that is a path to a file that can't run as is gets its interpreter or
# `open --` put in front of it.
_zenteru_accept_line() {
  local -a words reply
  words=(${(z)BUFFER})
  local w=${(Q)words[1]-} lead=${BUFFER%%[^[:space:]]*}
  [[ $w == '~' ]] && w=$HOME
  [[ $w == '~/'* ]] && w=$HOME/${w#\~/}
  if (( $#words == 1 )) && [[ -d $w ]] && ! whence -- "$w" >/dev/null; then
    BUFFER="cd -- ${BUFFER#$lead}"
  elif [[ $w == */* && -f $w && ! -x $w ]] && _zenteru_file_plan $w && (( $#reply > 1 )); then
    BUFFER="${(j: :)${(@q-)reply[1,-2]}} ${BUFFER#$lead}"
  fi
  zle _zenteru_prev_accept_line -- "$@"
}

# Tab in the first word with compinit: what it offered before, plus files.
_zenteru_command_files() {
  local ret=1
  "${_zenteru_command_prev:-_autocd}" "$@" && ret=0
  _files && ret=0
  return ret
}

# Installed at the first prompt, after the user's rc files, so a handler,
# accept-line widget or completion setup of theirs (made before or after
# this file loads) is kept and chained to.
_zenteru_local_install() {
  add-zsh-hook -d precmd _zenteru_local_install 2>/dev/null
  if (( ${+functions[command_not_found_handler]} )) &&
     [[ ${functions[command_not_found_handler]} != *_zenteru_local_handler* ]]; then
    functions[_zenteru_cnf_prev]=${functions[command_not_found_handler]}
  fi
  command_not_found_handler() { _zenteru_local_handler "$@"; }

  if [[ ${widgets[accept-line]-} != user:_zenteru_accept_line ]]; then
    zle -A accept-line _zenteru_prev_accept_line
    zle -N accept-line _zenteru_accept_line
  fi

  if (( ${+_comps} )); then
    if [[ ${_comps[-command-]-} != _zenteru_command_files ]]; then
      typeset -g _zenteru_command_prev=${_comps[-command-]:-_autocd}
      _comps[-command-]=_zenteru_command_files
    fi
  elif zmodload -i zsh/compctl 2>/dev/null; then
    # No compinit: compctl. Commands (aliases, functions, builtins and
    # reserved words included) and files.
    compctl -C -c -f
  fi
}
add-zsh-hook precmd _zenteru_local_install
