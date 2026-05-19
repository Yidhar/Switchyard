@echo off
title Switchyard GUI Launcher
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0start-gui.ps1"
