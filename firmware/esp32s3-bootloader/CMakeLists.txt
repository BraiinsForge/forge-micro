# Minimal ESP-IDF stub project. Its sole purpose is to build the ESP-IDF
# second-stage bootloader with rollback enabled (see sdkconfig.defaults);
# the application here is an empty placeholder because `idf.py bootloader`
# only links the bootloader subproject. The resulting bootloader.bin is
# flashed at offset 0x0 and drives OTA slot selection and auto-rollback
# for the Rust esp32s3 firmware.
cmake_minimum_required(VERSION 3.16)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(esp32s3_bootloader)
