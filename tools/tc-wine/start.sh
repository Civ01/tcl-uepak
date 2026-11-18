#!/bin/sh
export DISPLAY=:50 WINEDEBUG=-all WINEPREFIX=/tmp/wineprefix
unset TMPDIR
cd /home/claude/tctest/tc/portable
nohup wine TOTALCMD64.EXE /i=Z:\\home\\claude\\tctest\\wincmd.ini /f=Z:\\home\\claude\\tctest\\wcx_ftp.ini > /home/claude/tctest/tc.log 2>&1 &
