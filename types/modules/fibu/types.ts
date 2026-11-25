import { ns, min, partOf, indexed, unique, Ref, Enum, LocalDate, Money, Int } from "../../lib";

/** Ledger account of a chart of accounts. */
@ns("fibu")
export class LedgerAccount {
  @unique @indexed number!: string;
  @indexed name!: string;
  @indexed kind!: Enum<"asset" | "liability" | "equity" | "expense" | "revenue">;
}

/** Journal entry (double-entry booking record). */
@ns("fibu")
export class JournalEntry {
  @indexed documentDate!: LocalDate;
  @indexed postingDate!: LocalDate;
  @indexed documentNumber!: string;
  text?: string;
  fiscalYear!: Int;
}

/** One debit or credit line of a journal entry. */
@ns("fibu")
export class JournalLine {
  @partOf entry!: Ref<JournalEntry, "lines", "1..n">;
  account!: Ref<LedgerAccount, "lines">;
  side!: Enum<"debit" | "credit">;
  amount!: Money;
}
