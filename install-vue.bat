@echo off
set "PATH=C:\Program Files\nodejs;%PATH%"
cd /d C:\repos\firstmate
call "C:\Program Files\nodejs\npm.cmd" install vue naive-ui vue-virtual-scroller
call "C:\Program Files\nodejs\npm.cmd" install -D @vitejs/plugin-vue
