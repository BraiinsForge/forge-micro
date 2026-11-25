# Stub project whose only purpose is `idf.py bootloader`, which links the
# bootloader subproject alone and leaves the application a placeholder. The
# bootloader.bin it produces is flashed at offset 0x0 and drives OTA slot
# selection and auto-rollback for the esp32s3 firmware.
cmake_minimum_required(VERSION 3.16)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(esp32s3_bootloader)
