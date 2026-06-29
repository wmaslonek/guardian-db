import { Entity, PrimaryGeneratedColumn, Column, OneToMany, Index } from "typeorm";
import { User } from "./User";

@Entity({ name: "org" })
export class Org {
  @PrimaryGeneratedColumn()
  id!: number;

  @Index({ unique: true })
  @Column({ type: "text" })
  name!: string;

  @OneToMany(() => User, (user) => user.org)
  users!: User[];
}
