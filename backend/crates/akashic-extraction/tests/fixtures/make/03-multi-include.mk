include common.mk
-include optional.mk

a b: dep
	@echo building $@

dep: prereq1 \
     prereq2
	@echo done
