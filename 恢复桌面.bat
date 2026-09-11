@echo off
rem DeskFence stuck rescue: kill frozen instance, then restore native desktop icons.
rem DeskFence kong si zi jiu: sha diao ka si de shi li, hui fu yuan sheng zhuo mian tu biao.
taskkill /F /IM deskfence.exe >nul 2>&1
ping -n 2 127.0.0.1 >nul
"%~dp0target\release\deskfence.exe" --restore-desktop
