/* user/doom/c/freestanding_include/ctype.h: minimal freestanding ctype shim. */
#ifndef ARROST_FREESTD_CTYPE_H
#define ARROST_FREESTD_CTYPE_H

int isalpha(int c);
int isdigit(int c);
int isalnum(int c);
int isspace(int c);
int isprint(int c);
int isupper(int c);
int islower(int c);
int toupper(int c);
int tolower(int c);

#endif
