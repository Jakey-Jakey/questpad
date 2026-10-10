import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.*;
import ghidra.program.model.mem.*;
import ghidra.program.model.symbol.SourceType;

public class NrfSetup extends GhidraScript {
    void blk(String n, long a, long sz, boolean vol) throws Exception {
        Address ad = toAddr(a);
        if (currentProgram.getMemory().getBlock(ad) != null) return;
        MemoryBlock b = currentProgram.getMemory().createUninitializedBlock(n, ad, sz, false);
        b.setVolatile(vol); b.setWrite(true);
    }
    void lab(long a, String n) throws Exception {
        currentProgram.getSymbolTable().createLabel(toAddr(a), n, SourceType.USER_DEFINED);
    }
    void regs(long base, String p, Object... kv) throws Exception {
        for (int i = 0; i < kv.length; i += 2) lab(base + ((Number) kv[i]).longValue(), p + "_" + kv[i + 1]);
    }
    @Override
    public void run() throws Exception {
        for (MemoryBlock b : currentProgram.getMemory().getBlocks()) if (b.isInitialized()) { b.setWrite(false); b.setExecute(true); }
        blk("RAM", 0x20000000L, 0x40000, false);
        blk("FICR_UICR", 0x10000000L, 0x2000, false);
        blk("PERIPH", 0x40000000L, 0x30000, true);
        blk("PPB", 0xE0000000L, 0x100000, true);
        blk("P0", 0x50000000L, 0x1000, true);
        regs(0x40001000L, "RADIO", 0x0,"TASKS_TXEN",0x4,"TASKS_RXEN",0x8,"TASKS_START",0xC,"TASKS_STOP",0x10,"TASKS_DISABLE",
            0x100,"EVENTS_READY",0x104,"EVENTS_ADDRESS",0x108,"EVENTS_PAYLOAD",0x10C,"EVENTS_END",0x110,"EVENTS_DISABLED",
            0x114,"EVENTS_DEVMATCH",0x118,"EVENTS_DEVMISS",0x128,"EVENTS_BCMATCH",0x130,"EVENTS_CRCOK",0x134,"EVENTS_CRCERROR",
            0x200,"SHORTS",0x304,"INTENSET",0x308,"INTENCLR",0x400,"CRCSTATUS",0x408,"RXMATCH",0x40C,"RXCRC",
            0x504,"PACKETPTR",0x508,"FREQUENCY",0x50C,"TXPOWER",0x510,"MODE",0x514,"PCNF0",0x518,"PCNF1",
            0x51C,"BASE0",0x520,"BASE1",0x524,"PREFIX0",0x528,"PREFIX1",0x52C,"TXADDRESS",0x530,"RXADDRESSES",
            0x534,"CRCCNF",0x538,"CRCPOLY",0x53C,"CRCINIT",0x544,"TIFS",0x548,"RSSISAMPLE",0x550,"STATE",
            0x554,"DATAWHITEIV",0x560,"BCC",0x600,"DAB0",0x620,"DAP0",0x640,"DACNF",0x650,"MODECNF0",0xFFC,"POWER");
        regs(0x4000F000L, "CCM", 0x0,"TASKS_KSGEN",0x4,"TASKS_CRYPT",0x8,"TASKS_STOP",0x100,"EVENTS_ENDKSGEN",
            0x104,"EVENTS_ENDCRYPT",0x108,"EVENTS_ERROR",0x200,"SHORTS",0x304,"INTENSET",0x308,"INTENCLR",
            0x400,"MICSTATUS",0x500,"ENABLE",0x504,"MODE",0x508,"CNFPTR",0x50C,"INPTR",0x510,"OUTPTR",0x514,"SCRATCHPTR");
        regs(0x4000E000L, "ECB", 0x0,"TASKS_STARTECB",0x4,"TASKS_STOPECB",0x100,"EVENTS_ENDECB",0x104,"EVENTS_ERRORECB",0x504,"ECBDATAPTR");
        regs(0x4000D000L, "RNG", 0x0,"TASKS_START",0x4,"TASKS_STOP",0x100,"EVENTS_VALRDY",0x504,"CONFIG",0x508,"VALUE");
        regs(0x10000000L, "FICR", 0x60,"DEVICEID0",0x64,"DEVICEID1",0x80,"ER0",0x90,"IR0",0xA0,"DEVICEADDRTYPE",0xA4,"DEVICEADDR0",0xA8,"DEVICEADDR1",0x100,"INFO_PART",0x104,"INFO_VARIANT");
        for (int i = 0; i < 32; i++) lab(0x10001080L + 4 * i, "UICR_CUSTOMER" + i);
    }
}
