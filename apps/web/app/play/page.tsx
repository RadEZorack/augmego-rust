import { auth } from "@/src/auth";
import { loadAuthUser } from "@/src/lib/auth-user";
import PlayClient from "@/src/components/play/PlayClient";

export default async function PlayPage() {
  const session = await auth();
  const user = session?.user?.id ? await loadAuthUser(session.user.id) : null;

  return <PlayClient initialUser={user} />;
}
