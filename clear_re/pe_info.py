import struct

PATH = r"D:\Users\Syntac\Documents\OBS\VST3\Clear.vst3"
data = open(PATH, "rb").read()

lf = struct.unpack_from("<I", data, 0x3C)[0]
sig, machine, nsec, ts, symtab, nsyms, opth, chars = struct.unpack_from("<4sHHIIIHH", data, lf)
print(f"PE header @ {lf:#x} signature={sig!r}")
print(f"Machine={machine:#06x} Sections={nsec} TimeDateStamp={ts}")
print(f"SizeOfOptionalHeader={opth:#x} Characteristics={chars:#06x}")

opt = lf + 4 + 20
magic, maj, mnr, soc, soi, sou, entry, boc = struct.unpack_from("<HBBIIIII", data, opt)
base = struct.unpack_from("<Q", data, opt + 24)[0]
print(f"Optional magic={magic:#06x} linker={maj}.{mnr} entryRVA={entry:#x} imageBase={base:#x}")

dd = opt + 112
DD = ["EXPORT", "IMPORT", "RESOURCE", "EXCEPTION", "SECURITY", "BASERELLOC", "DEBUG",
      "ARCHITECTURE", "TLS", "LOAD_CONFIG", "BOUND_IMP", "IAT", "DELAY_IMP", "CLR_RUNTIME"]
print("\nData directories:")
for i, n in enumerate(DD):
    rva, size = struct.unpack_from("<II", data, dd + i * 8)
    if rva or size:
        print(f"  {n:12s} RVA={rva:#010x} size={size:#08x}")

sec_off = dd + 128
sections = []
print("\nSections:")
for i in range(nsec):
    e = data[sec_off + i * 40: sec_off + i * 40 + 40]
    name = e[:8].rstrip(b"\x00").decode("latin1")
    vsize, vaddr, rawsize, rawptr, flags = struct.unpack_from("<IIII4xI", e, 8)
    sections.append((name, vsize, vaddr, rawsize, rawptr, flags))
    print(f"  {name:8s} VA={vaddr:#010x} VSize={vsize:#010x} Raw={rawsize:#010x} Ptr={rawptr:#010x} Flags={flags:#010x}")

def rva_to_off(rva):
    for name, vsize, vaddr, rawsize, rawptr, flags in sections:
        if vaddr <= rva < vaddr + max(vsize, rawsize):
            return rawptr + (rva - vaddr)
    return None

def cstr(p, limit=80):
    return data[p:p + limit].split(b"\x00")[0].decode("utf-8", "replace")

# Debug / PDB
rva, size = struct.unpack_from("<II", data, dd + 6 * 8)
print("\nDebug:")
if rva:
    off = rva_to_off(rva)
    if off:
        f2, t2, mj, mn, typ, sz, addr, ptr = struct.unpack_from("<IIHHIIII", data, off)
        print(f"  type={typ} size={sz} ptr={ptr:#x}")
        if ptr:
            print("  PDB: " + cstr(ptr, sz or 200))
    else:
        print("  not in file data")

# Exports
rva, size = struct.unpack_from("<II", data, dd + 0 * 8)
print("\nExports:")
if rva:
    off = rva_to_off(rva)
    if off:
        ch, t, mj, mn, nname, base, nfun, nnames, funs, names, ords = struct.unpack_from("<IIHHIIIIIII", data, off)
        print("  name: " + cstr(rva_to_off(nname)))
        print(f"  functions={nfun} names={nnames}")
        foff, noff, ooff = rva_to_off(funs), rva_to_off(names), rva_to_off(ords)
        if not (foff and noff and ooff):
            print("  export arrays point outside file data (packed section)")
        else:
            for i in range(nnames):
                nrva = struct.unpack_from("<I", data, noff + i * 4)[0]
                orow = struct.unpack_from("<H", data, ooff + i * 2)[0]
                sym = struct.unpack_from("<I", data, foff + orow * 4)[0]
                print(f"    [{base + orow}] {cstr(rva_to_off(nrva))} -> RVA {sym:#x}")
    else:
        print("  not in file data")

# Imports
rva, size = struct.unpack_from("<II", data, dd + 1 * 8)
print("\nImports:")
if rva:
    off = rva_to_off(rva)
    if off:
        while True:
            ilt, tstamp, forw, name_rva, iat = struct.unpack_from("<IIIII", data, off)
            if not name_rva:
                break
            print("  " + cstr(rva_to_off(name_rva)))
            tl = ilt or iat
            tlo = rva_to_off(tl)
            if tlo:
                syms = []
                for k in range(300):
                    v = struct.unpack_from("<Q", data, tlo + k * 8)[0]
                    if v == 0:
                        break
                    if v >> 63:
                        syms.append(f"ord{v & 0xFFFF}")
                    else:
                        so = rva_to_off(v)
                        syms.append(cstr(so + 2) if so else "?")
                print("    " + ", ".join(syms))
            off += 20
    else:
        print("  not in file data (packed)")

# Resources (version info)
rva, size = struct.unpack_from("<II", data, dd + 2 * 8)
print("\nResources / Version info:")
roff = rva_to_off(rva) if rva else None
if roff:
    def read_wstr(p):
        out = bytearray()
        while p + 1 < len(data) and data[p:p + 2] != b"\x00\x00":
            out += data[p:p + 2]
            p += 2
        return out.decode("utf-16-le", "replace"), p + 2

    def walk_var(p, end, depth=0):
        wLength, wValueLength, wType = struct.unpack_from("<HHH", data, p)
        if wLength == 0:
            return
        key, q = read_wstr(p + 6)
        if wValueLength:
            val = data[q:q + wValueLength]
            if wType == 1:
                print("  " * depth + f"{key} = {val.decode('utf-16-le', 'replace')}")
            else:
                sig, sv, fms, fls, pms, pls, fmask, flags, fos = struct.unpack_from("<IIIIIIIII", val, 0)
                print("  " * depth + f"{key}: file version {fms}.{fls} product version {pms}.{pls} (sig={sig:#010x})")
            q += wValueLength
        q = (q + 3) & ~3
        while q < p + wLength:
            walk_var(q, p + wLength, depth + 1)
            q = (q + 3) & ~3

    def walk_dir(p, depth=0):
        named, ids = struct.unpack_from("<HH", data, p + 18)
        n = named + ids
        for i in range(n):
            name, off = struct.unpack_from("<II", data, p + 16 + i * 8)
            if name & 0x80000000:
                label, _ = read_wstr(roff + (name & 0x7FFFFFFF))
            else:
                label = str(name)
            if off & 0x80000000:
                print("  " * depth + f"[{label}]")
                walk_dir(roff + (off & 0x7FFFFFFF), depth + 1)
            else:
                sz, drva, cp, tstamp = struct.unpack_from("<IIII", data, roff + off)
                print("  " * depth + f"[{label}] size={sz}")
                doff = rva_to_off(drva)
                if doff:
                    walk_var(doff, doff + sz, depth + 1)

    walk_dir(roff)

