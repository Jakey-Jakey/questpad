import ghidra.app.script.GhidraScript;
import ghidra.app.cmd.disassemble.ArmDisassembleCommand;
import ghidra.program.model.address.*;
import ghidra.program.model.listing.*;
import ghidra.program.model.mem.*;
import java.util.*;

// Create functions at every Thumb BL target and every odd (Thumb) pointer into code.
public class ForceFuncs extends GhidraScript {
    @Override
    public void run() throws Exception {
        Memory mem = currentProgram.getMemory();
        Listing lst = currentProgram.getListing();
        TreeSet<Long> targets = new TreeSet<>();
        for (MemoryBlock b : mem.getBlocks()) {
            if (!b.isInitialized() || !b.isExecute()) continue;
            long s = b.getStart().getOffset(), e = b.getEnd().getOffset();
            byte[] d = new byte[(int) (e - s + 1)];
            b.getBytes(b.getStart(), d);
            for (int i = 0; i + 3 < d.length; i += 2) {
                int h1 = (d[i] & 0xff) | (d[i + 1] & 0xff) << 8;
                int h2 = (d[i + 2] & 0xff) | (d[i + 3] & 0xff) << 8;
                if ((h1 >> 11) == 0x1e && (h2 & 0xd000) == 0xd000) {
                    int S = (h1 >> 10) & 1, imm10 = h1 & 0x3ff, J1 = (h2 >> 13) & 1, J2 = (h2 >> 11) & 1, imm11 = h2 & 0x7ff;
                    int I1 = 1 - (J1 ^ S), I2 = 1 - (J2 ^ S);
                    int off = (S << 24) | (I1 << 23) | (I2 << 22) | (imm10 << 12) | (imm11 << 1);
                    if (S == 1) off -= (1 << 25);
                    long t = s + i + 4 + off;
                    if (t >= s && t <= e) targets.add(t);
                }
                if ((i & 3) == 0) {
                    long v = ((d[i] & 0xffL) | (d[i + 1] & 0xffL) << 8 | (d[i + 2] & 0xffL) << 16 | (d[i + 3] & 0xffL) << 24);
                    if ((v & 1) == 1 && v - 1 >= s && v - 1 <= e) targets.add(v - 1);
                }
            }
        }
        int made = 0;
        for (long t : targets) {
            Address a = toAddr(t);
            if (lst.getFunctionAt(a) != null) continue;
            Function f = lst.getFunctionContaining(a);
            if (f != null && !f.getEntryPoint().equals(a)) continue;
            Data dd = lst.getDefinedDataAt(a);
            if (dd != null) continue;
            if (lst.getInstructionAt(a) == null) {
                ArmDisassembleCommand cmd = new ArmDisassembleCommand(a, null, true);
                cmd.applyTo(currentProgram, monitor);
            }
            if (lst.getInstructionAt(a) != null && createFunction(a, null) != null) made++;
        }
        println("ForceFuncs: candidate targets " + targets.size() + ", created " + made);
    }
}
