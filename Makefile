#
# We differentiate between release / debug build types using the BUILD_TYPE
# environment variable.
#
BUILD_TYPE ?= debug
ifeq ($(BUILD_TYPE),release)
	CARGO_BUILD_FLAGS += --release
endif

.PHONY: all
all: xactserver


#
# Transaction server
#

.PHONY: xactserver
xactserver:
	+@echo "Compiling xactserver"
	cargo build $(CARGO_BUILD_FLAGS)


# This doesn't remove the effects of 'configure'.
.PHONY: clean
clean:
	cargo clean

# This removes everything
.PHONY: distclean
distclean:
	cargo clean
