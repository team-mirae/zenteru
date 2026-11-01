# zenteru: run a script in the current directory by its bare name.
#
#   % deploy.sh --dry-run        # no `deploy.sh` command anywhere
#   zenteru: running ./deploy.sh
#
# This is a command-not-found handler, so it only ever runs when no command,
# function, alias or builtin of that name exists: it can't shadow a real
# command. The tradeoff is that a mistyped command can run a local file of
# that name. Only bare names (no `/`) of regular files in $PWD qualify;
# executables run as ./NAME, others with their #! interpreter, else .sh with
# sh, .zsh with zsh and .bash with bash. Anything else falls through to the
# handler you had before (Homebrew's command-not-found, say) or the usual
# "command not found".

# Command line for running $1 from $PWD, in reply; status 1 if it doesn't qualify.
_zenteru_local_plan() {
  local name=$1 first=
  reply=()
  [[ -n $name && $name != */* && -f ./$name ]] || return 1
  if [[ -x ./$name ]]; then
    reply=(./$name)
    return 0
  fi
  IFS= read -r first < ./$name 2>/dev/null
  if [[ $first == '#!'* ]]; then
    reply=(${=${first#\#!}} ./$name)
    (( $#reply > 1 )) && return 0
  fi
  case $name in
    *.sh) reply=(sh ./$name) ;;
    *.zsh) reply=(zsh ./$name) ;;
    *.bash) reply=(bash ./$name) ;;
    *) reply=(); return 1 ;;
  esac
}

_zenteru_local_handler() {
  local -a reply
  if _zenteru_local_plan "$1"; then
    shift
    print -u2 -r -- $'\e[2m'"zenteru: running ${(j: :)${(@q-)reply}}"$'\e[22m'
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

# Installed at the first prompt, after the user's rc files, so a handler they
# define (before or after this file loads) is kept and chained to.
_zenteru_local_install() {
  add-zsh-hook -d precmd _zenteru_local_install 2>/dev/null
  if (( ${+functions[command_not_found_handler]} )) &&
     [[ ${functions[command_not_found_handler]} != *_zenteru_local_handler* ]]; then
    functions[_zenteru_cnf_prev]=${functions[command_not_found_handler]}
  fi
  command_not_found_handler() { _zenteru_local_handler "$@"; }
}
add-zsh-hook precmd _zenteru_local_install
